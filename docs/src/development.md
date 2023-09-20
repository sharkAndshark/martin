# Development

Clone Martin, setting remote name to `upstream`. This way `main` branch will be updated automatically with the latest changes from the upstream repo.

```shell, ignore
git clone https://github.com/maplibre/martin.git -o upstream
cd martin
```

Fork Martin repo into your own GitHub account, and add your fork as a remote

```shell, ignore
git remote add origin  _URL_OF_YOUR_FORK_
```

Install [docker](https://docs.docker.com/get-docker/), [docker-compose](https://docs.docker.com/compose/), and [openssl](https://www.openssl.org/):

```shell, ignore
# For Ubuntu-based distros
sudo apt install -y  docker.io  docker-compose  libssl-dev  build-essential  jq pkg-config
```

Install [Just](https://github.com/casey/just#readme) (improved makefile processor). Note that some Linux and Homebrew distros have outdated versions of Just, so you should install it from source:

```shell, ignore
cargo install just
```

When developing MBTiles SQL code, you may need to use `just prepare-sqlite` whenever SQL queries are modified. Run `just` to see all available commands:

```shell, ignore
❯ just
Available recipes:
    run *ARGS              # Start Martin server
    run-release *ARGS      # Start release-compiled Martin server and a test database
    debug-page *ARGS       # Start Martin server and open a test page
    psql *ARGS             # Run PSQL utility against the test database
    pg_dump *ARGS          # Run pg_dump utility against the test database
    clean                  # Perform  cargo clean  to delete all build files
    start                  # Start a test database
    start-ssl              # Start an ssl-enabled test database
    start-legacy           # Start a legacy test database
    restart                # Restart the test database
    stop                   # Stop the test database
    bench                  # Run benchmark tests
    bench-http             # Run HTTP requests benchmark using OHA tool. Use with `just run-release`
    test                   # Run all tests using a test database
    test-ssl               # Run all tests using an SSL connection to a test database. Expected output won't match.
    test-legacy            # Run all tests using the oldest supported version of the database
    test-unit *ARGS        # Run Rust unit and doc tests (cargo test)
    test-int               # Run integration tests
    bless                  # Run integration tests and save its output as the new expected output
    book                   # Build and open mdbook documentation
    package-deb            # Build debian package
    docs                   # Build and open code documentation
    coverage FORMAT='html' # Run code coverage on tests and save its output in the coverage directory. Parameter could be html or lcov.
    docker-build           # Build martin docker image
    docker-run *ARGS       # Build and run martin docker image
    git *ARGS              # Do any git command, ensuring that the testing environment is set up. Accepts the same arguments as git.
    print-conn-str         # Print the connection string for the test database
    lint                   # Run cargo fmt and cargo clippy
    fmt                    # Run cargo fmt
    fmt2                   # Run Nightly cargo fmt, ordering imports
    clippy                 # Run cargo clippy
    prepare-sqlite         # Update sqlite database schema.
```
