# How to run it?

## Getting a compiled version

Each release comes with a compiled version for Linux x86 64 musl. This binary is stripped, optimized and should run on any Linux server with the
said architecture regardless of the `libc` used in the distribution as the binary comes with its own `libc`. However, if you want it for some other 
platform, you can [install the Rust toolchain](https://www.rust-lang.org/learn/get-started) and then `cargo build -r` in the project's directory
to get an optimized release binary for your platform.

## Running the loginbot

### Prerequirements

 - A server to run loginbot on it. Root access is NOT required nor it is recommended to run loginbot as root.
 - A webserver like nginx to act as reverse proxy so that the outside world can talk with loginbot web APIs.
 - An email account for the loginbot

### Steps

 - Download the latest release from Releases and get the binary
 - Modify the TOML config file to your need. You need an email account somewhere for the loginbot
 - The `oauth` section of the configuration file must have the same values as in your website(e.g. Discourse) in the OAuth2 section. client id and secret should be randomly generated strings for the production and you must not leak them to untrusted parties.
 - Run the binary. By default, it looks for `config.toml` in the current directory but you can specify somewhere else by a command line argument: `./loginbot /path/to/config.toml`
 - For production runs, a service manager is highly recommended for easier management of the loginbot process. For instance on Debian based distros and many others, systemd is used.
 - The bot's web API is listening to `listen_addr` as specified in the config file. You need your webserver to act as a reverse proxy for the login bot.
