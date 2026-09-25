install:
    cargo install sqlx-cli
    sqlite3 minx.db ".read ./database/schema.sql"
    cargo sqlx prepare --database-url sqlite:minx.db

run:
    cargo run

build:
    cargo build
