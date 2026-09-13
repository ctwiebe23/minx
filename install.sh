# clone repo
git clone https://github.com/ctwiebe23/minx
cd minx/
# update rust
rustup update
# prep db
cargo install sqlx-cli
sqlite3 minx.db ".read schema.sql"
cargo sqlx prepare --database-url sqlite:minx.db
# run
cargo run
# if port 80 is desired but sudo cargo is not
sudo apt install authbind
# give all users/groups access to 80
sudo touch /etc/authbind/byport/80
sudo chmod 777 /etc/authbind/byport/80
authbind --deep cargo run
