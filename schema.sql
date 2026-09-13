drop table if exists locator_keyword;
drop table if exists locator;
drop table if exists keyword;

pragma foreign_keys = on;

create table keyword
( id        integer primary key
, content   text not null unique
);

create table locator
( id        integer primary key
, content   text not null unique
, visits    integer not null default 0
);

create table locator_keyword
( id            integer primary key
, locator_id    integer not null
, keyword_id    integer not null
, unique(locator_id, keyword_id)
, foreign key (locator_id) references locator(id) on delete cascade
, foreign key (keyword_id) references keyword(id) on delete cascade
);
