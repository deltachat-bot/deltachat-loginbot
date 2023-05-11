# DeltaChat loginbot for OAuth2

This is an OAuth2 API provider for DeltaChat. OAuth2 is one of the APIs behind the buttons such as "Login with Google". On supported websites such as Discourse, the users can login with the providers such as Google, Facebook, Github, Gitlab and others without creating an account on the website(e.g. your Discourse forum).

This loginbot provides a [DeltaChat bot](https://delta.chat) and a web API so that users can login with their DeltaChat account.

## How does it work?

1. The front-end first sends a GET request to the `/requestQr` API of this loginbot to create and get an invite to a DeltaChat group.
2. The API returns an [OpenPGP4FPR](https://github.com/deltachat/interface/blob/master/uri-schemes.md#openpgp4fpr-) link with which the user can join the group with their DeltaChat app.
3. The front-end calls `/checkStatus` API on an interval to check if the user has joined the group.
4. Once the user joins the group, the above API returns a success message and the user, the user's contact ID will be written into the session data.
5. When the front-end gets the success message, it will open `/authorize` page in the user's browser from where they will be redirected to the website's(for example Discourse's) callback URL.
6. Later, the website will check `/token` API in the login bot to check if the user has really authenticated themselves with our loginbot and that the call to its callback is not fake.

## Technologies used

 - [tide](https://github.com/http-rs/tide) for the web API part
 - [DeltaChat core](https://github.com/deltachat/deltachat-core-rust)
 - [sled-rs](https://sled.rs/) as the Key-Value database
 - TOML for the config file

## License

This project is under copyright of Farooq Karimi Zadeh and DeltaChat team. Some rights are reserved under Affero General Public License version 3 or at your option any later version as published by the Free Software Foundation. You should have received a copy of this license in the LICENSE file in this git repository.
