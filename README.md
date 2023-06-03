# DeltaChat loginbot for OAuth2

This is an OAuth2 API provider for DeltaChat. OAuth2 is one of the APIs behind the buttons such as "Login with Google". On supported websites such as Discourse, the users can login with the providers such as Google, Facebook, Github, Gitlab and others without creating an account on the website(e.g. your Discourse forum).

This loginbot provides a [DeltaChat bot](https://delta.chat) and a web API so that users can login with their DeltaChat account.

## What is the principle behind "Login with DeltaChat"?
"Login with DeltaChat" uses the Secure join protocol to authenticate a user.

When you visit the website and you are not logged in you get to the login page. From there, you are prompted to scan the DeltaChat QR code to login, which is internally just a DeltaChat group invite. Once you join the group the bot knows who you are (no extra email verification needed, because emails were already exchanged in this process). Your session is now authenticated.

In our case of this login bot, the website you access is an OAuth2 provider. So you can use this as a login method for all kinds of web-apps like [Discourse](https://www.discourse.org/) or [Wiki.js](https://js.wiki) which both have a generic OAuth2 authentication module.

## How does it work?

1. The front-end first sends a GET request to the `/requestQr` API of this loginbot to create and get an invite to a DeltaChat group.
2. The API returns an [OpenPGP4FPR](https://github.com/deltachat/interface/blob/master/uri-schemes.md#openpgp4fpr-) link with which the user can join the group with their DeltaChat app.
3. The front-end calls `/checkStatus` API on an interval to check if the user has joined the group.
4. Once the user joins the group, the above API returns a success message and the user, the user's contact ID will be written into the session data.
5. When the front-end gets the success message, it will open `/authorize` page in the user's browser from where they will be redirected to the website's(for example Discourse's) callback URL.
6. Later, the website will check `/token` API in the login bot to check if the user has really authenticated themselves with our loginbot and that the call to its callback is not fake.

## Technologies used

 - [axum](https://github.com/tokio-rs/axum) for the web API part
 - [DeltaChat core](https://github.com/deltachat/deltachat-core-rust)
 - [sled-rs](https://sled.rs/) as the Key-Value database
 - TOML for the config file

## How to run it?

### Pre-built binaries (only for Linux `x86_64` `musl`)

 - Download the latest release from Releases
 - Extract the compressed file
 - Modify the TOML config file to your need. You need an email account somewhere for the loginbot
 - The `oauth` section of the config file must be parallel with your website configuration.
 - Run the binary. By default it looks for The `oauth` section of the config file must be parallel with your website configuration.
 - Run the binary. By default, it looks for `config.toml` in the current directory but you can specify somewhere else by a command line argument: `./loginbot /path/to/config.toml`
 - For production runs, a service manager is highly recommended for easier management of the loginbot process. For instance on Debian based distros and many others, systemd is used.
 - There is no need for loginbot to be run as root. You can use systemd user services for instance.
 - The bot's web APIBy default, it looks for `config.toml` in the current directory but you can specify somewhere else by a command line argument: `./loginbot /path/to/config.toml`
 - For production runs, a service manager is highly recommended for easier management of the loginbot process. For instance on Debian based distros and many others, systemd is used.
 - There is no need for loginbot to be run as root. You can use systemd user services for instance.
 - The bot's web API is listening to `listen_addr` as specified in the config file. You need your webserver to act as a reverse proxy for the login bot.

### Building from source

TL;DR Clone this repo, checkout to latest release, compile using cargo and follow the points 3 and afterwards of the previous section

## License

This project is under copyright of Farooq Karimi Zadeh and DeltaChat team. Some rights are reserved under Affero General Public License version 3 or at your option any later version as published by the Free Software Foundation. You should have received a copy of this license in the LICENSE file in this git repository.
