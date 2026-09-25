use std::collections::BTreeSet;

use maud::{Markup, html};
use sqlx::{
    QueryBuilder, Sqlite, SqliteConnection, SqlitePool, query, query_scalar,
    sqlite::SqliteQueryResult,
};
use url::Url;

type Keywords = BTreeSet<String>;
type Locator = String;

#[derive(sqlx::FromRow, Debug)]
struct LocatorModel {
    id: i64,
    content: String,
    visits: i64,
    keywords: String,
}

#[derive(Debug)]
pub enum Command {
    NoOp,
    Search(Keywords),
    Alter(Locator, Keywords),
    Delete(Locator),
    Increment(Locator),
}

#[derive(Debug)]
pub enum CommandResult {
    NavigateTo(Locator),
    SearchFailed,
    Deleted,
    InternalError,
    NothingHappened,
    Incremented,
}

pub fn terms_to_keywords(terms: &str) -> Keywords {
    terms
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect()
}

pub fn terms_to_command(terms: &str) -> Command {
    let keywords = terms_to_keywords(terms);

    if keywords.len() == 0 {
        return Command::NoOp;
    }

    let first = terms.split_whitespace().next().unwrap_or("");

    if let Ok(locator) = Url::parse(first) {
        return Command::Alter(locator.as_str().to_owned(), keywords);
    } else {
        return Command::Search(keywords);
    }
}

async fn get_locator_id(locator: &str, conn: &mut SqliteConnection) -> Result<i64, sqlx::Error> {
    query_scalar!(
        r#"
        insert into locator(content) values ($1)
        on conflict(content) do update set content = content
        returning id;
        "#,
        locator,
    )
    .fetch_one(conn)
    .await
}

async fn get_keyword_id(keyword: &str, conn: &mut SqliteConnection) -> Result<i64, sqlx::Error> {
    query_scalar!(
        r#"
        insert into keyword(content) values ($1)
        on conflict(content) do update set content = content
        returning id;
        "#,
        keyword,
    )
    .fetch_one(conn)
    .await
}

async fn increment_locator_visits_by_id(
    locator_id: i64,
    pool: &SqlitePool,
) -> Result<SqliteQueryResult, sqlx::Error> {
    query!(
        r#"
        update locator set visits = visits + 1 where id = $1
        "#,
        locator_id
    )
    .execute(pool)
    .await
}

async fn insert_join(
    locator_id: i64,
    keyword_id: i64,
    conn: &mut SqliteConnection,
) -> Result<SqliteQueryResult, sqlx::Error> {
    query!(
        r#"
        insert or ignore into locator_keyword(locator_id, keyword_id)
        values ($1, $2);
        "#,
        locator_id,
        keyword_id
    )
    .execute(conn)
    .await
}

async fn delete_locator(
    locator: &str,
    conn: &mut SqliteConnection,
) -> Result<SqliteQueryResult, sqlx::Error> {
    query!(
        r#"
        delete from locator where content = $1;
        "#,
        locator
    )
    .execute(conn)
    .await
}

async fn alter(
    locator: &Locator,
    keywords: &Keywords,
    pool: &SqlitePool,
) -> Result<(), sqlx::Error> {
    let locator = locator.as_str();
    let mut transaction = pool.begin().await?;

    // Delete the old locator first to clean out incorrect keywords relations
    delete_locator(locator, &mut transaction).await?;
    let locator_id = get_locator_id(locator, &mut transaction).await?;
    for keyword in keywords {
        let keyword_id = get_keyword_id(keyword, &mut transaction).await?;
        insert_join(locator_id, keyword_id, &mut transaction).await?;
    }

    transaction.commit().await?;

    increment_locator_visits_by_id(locator_id, pool).await?;
    Ok(())
}

async fn get_locators_by_keywords(
    keywords: &Keywords,
    pool: &SqlitePool,
) -> Result<Vec<LocatorModel>, sqlx::Error> {
    if keywords.len() == 0 {
        return Ok(Vec::new());
    }

    let mut query: QueryBuilder<Sqlite> = QueryBuilder::new(
        r#"
        with match as (
        "#,
    );
    let mut first_time = true;

    for keyword in keywords {
        if first_time {
            first_time = false;
        } else {
            query.push(" intersect ");
        }

        query.push(
            r#"
            select distinct l.id
            from locator l
            join locator_keyword j on l.id = j.locator_id
            join keyword k on k.id = j.keyword_id
            where k.content like 
            "#,
        );
        query.push_bind(format!("{keyword}%"));
    }

    query.push(
        r#"
        )
        select l.id, l.content, l.visits, group_concat(k.content, ' ') as keywords
        from locator l
        join match m on m.id = l.id
        join locator_keyword j on l.id = j.locator_id
        join keyword k on k.id = j.keyword_id
        group by l.id
        "#,
    );

    let mut matches = query.build_query_as::<LocatorModel>().fetch_all(pool).await;
    if matches.is_ok() {
        sort_matches(matches.as_mut().unwrap(), keywords);
    }
    matches
}

async fn search(keywords: &Keywords, pool: &SqlitePool) -> Result<Option<String>, sqlx::Error> {
    if let Some(locator) = get_locators_by_keywords(keywords, pool).await?.get(0) {
        increment_locator_visits_by_id(locator.id, pool).await?;
        Ok(Some(locator.content.clone()))
    } else {
        Ok(None)
    }
}

async fn increment(locator: &Locator, pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let locator = locator.as_str();
    let mut conn = pool.acquire().await?;
    let id = get_locator_id(locator, &mut conn).await?;
    increment_locator_visits_by_id(id, pool).await?;
    Ok(())
}

async fn delete(locator: &Locator, pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let locator = locator.as_str();
    let mut transaction = pool.begin().await?;

    delete_locator(locator, &mut transaction).await?;

    transaction.commit().await?;
    Ok(())
}

pub async fn dispatch(command: Command, pool: &SqlitePool) -> CommandResult {
    return match command {
        Command::Alter(locator, keywords) => match alter(&locator, &keywords, &pool).await {
            Ok(()) => CommandResult::NavigateTo(locator),
            Err(_) => CommandResult::InternalError,
        },
        Command::Search(keywords) => match search(&keywords, &pool).await {
            Ok(Some(locator)) => CommandResult::NavigateTo(locator),
            Ok(None) => CommandResult::SearchFailed,
            Err(_) => CommandResult::InternalError,
        },
        Command::Delete(locator) => match delete(&locator, &pool).await {
            Ok(()) => CommandResult::Deleted,
            Err(_) => CommandResult::InternalError,
        },
        Command::Increment(locator) => match increment(&locator, &pool).await {
            Ok(()) => CommandResult::Incremented,
            Err(_) => CommandResult::InternalError,
        },
        Command::NoOp => CommandResult::NothingHappened,
    };
}

fn sort_matches(matches: &mut Vec<LocatorModel>, keywords: &Keywords) {
    matches.sort_by_key(|m| {
        let mut mkeywords = m.keywords.split(" ");
        let perfect_matches: i32 = keywords
            .iter()
            .filter(|kw| mkeywords.any(|mkw| kw == &mkw))
            .collect::<Vec<_>>()
            .len()
            .try_into()
            .unwrap_or(0);
        let imperfect_matches: i32 =
            mkeywords.collect::<Vec<_>>().len().try_into().unwrap_or(0) - perfect_matches;
        let relevence_group = if imperfect_matches < 3 {
            imperfect_matches
        } else if imperfect_matches == 3 {
            2
        } else {
            3
        };
        (
            -perfect_matches.try_into().unwrap_or(0),
            relevence_group,
            -m.visits,
        )
    });
}

pub async fn make_matches(keywords: &Keywords, pool: &SqlitePool) -> Markup {
    if keywords.len() == 0 {
        return html! {};
    }

    if let Ok(matches) = get_locators_by_keywords(keywords, pool).await {
        if matches.len() == 0 {
            html! {
                p { "No matches" }
            }
        } else {
            html! {
                ol {
                    @for m in matches {
                        li _={ "on click fetch '/' + encodeURIComponent('" (m.content) "') with method: 'PUT' then go to " (m.content) } {
                            h2 { (m.content) }
                            div .visits { (m.visits) }
                            p { ({
                                let url_keywords = terms_to_keywords(&m.content);
                                m.keywords.split(" ").filter(|k| !url_keywords.contains(k.to_owned())).collect::<Vec<_>>().join(" ")
                            }) }
                            button _={ "on click set #terms's value to the previous <h2/>'s innerText + ' ' + the previous <p/>'s innerText then halt the event" } {
                                "edit"
                            }
                            button _={ "on click trigger delete(locator: '" (m.content) "') then halt the event" } {
                                "delete"
                            }
                        }
                    }
                }
            }
        }
    } else {
        html! {
            p { "Internal Server Error" }
        }
    }
}
