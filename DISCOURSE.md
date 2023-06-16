# How to run loginbot for Discourse?

The loginbot implements the OAuth2 specification partially and it covers the API [Discourse](https://www.discourse.org/) needs. Therefore, you can use it for 
adding "Login with DeltaChat" to your Discourse instance. 

## Getting a compiled version

Each release comes with a compiled version for Linux x86 64 musl. This binary is stripped, optimized and should run on any Linux server with the
said architecture regardless of the `libc` used in the distribution as the binary comes with its own `libc`. 


## Compiling from source

If you want it for some other 
platform, you can [install the Rust toolchain](https://www.rust-lang.org/learn/get-started) and then `cargo build -r` in the project's directory
to get an optimized release binary for your platform.

## Prerequirements

1. An email account for the loginbot
2. Admin access to the Discourse instance
3. A server to run loginbot on
  - With a public IP address
  - Root access is NOT required nor it is recommended to run loginbot as root.
  - A webserver like nginx to act as reverse proxy so that the outside world can talk with loginbot web APIs.
4. A domain you can point to the webserver

## Steps

The first step assumes you are on Linux and you are fine with a statically linked binary with musl libc.

 1. Download the latest release from Releases and get the binary: `wget https://github.com/deltachat-bot/deltachat-loginbot/releases/download/v0.3.0/deltachat-loginbot-x86_64-linux-musl_0.3.0.tar.bz2`
 2. Extract loginbot's binary and other stuff to an empty directory: `tar xvf deltachat-loginbot-x86_64-linux-musl_0.3.0.tar.bz2 -C empty_directory`
 3. Rename `example_config.toml` to `config.toml` and modify the TOML config file(`config.toml`) to your need. Enter the email address and password for the loginbot account into the `email` and `password` fields in `config.toml`
 4. Use `bash scripts/gen_secret.sh` to generate two random strings for `client_id` and `client_secret` in `oauth` section of the config file. Enter one of randomly generated strings in `client_id` field and the other in `client_secret` field.
 5. Enter `https://discourse.tld/auth/oauth2_basic/callback` as `redirect_uri` where `discouse.tld` is the domain address of your Discourse.
 6. Run the binary. By default, it looks for `config.toml` in the current directory but you can specify somewhere else by a command line argument: `./loginbot /path/to/config.toml`
 7. For production runs, a service manager is highly recommended for easier management of the loginbot process. For instance on Debian based distros and many others, systemd is used. You can use the systemd user service template(`loginbot.service`).
 8. The bot's web API is listening to `listen_addr` as specified in the config file. To see different possible values for `listen_addr` see [valid values for SocketAddr](https://doc.rust-lang.org/nightly/core/net/enum.SocketAddr.html).
9. You need a webserver to act as a reverse proxy for the login bot (see [this guide](https://www.digitalocean.com/community/tutorials/how-to-configure-nginx-as-a-reverse-proxy-on-ubuntu-22-04) as an example). Let it proxy traffic to the `listen_addr` from the `config.toml`.
10. Point your domain to the server, so users can reach it (e.g. with `foo.com`).
11. Create a TLS certificate for your domain, so the reverse proxy can encrypt the connection to the user, e.g. with [certbot](https://certbot.eff.org/).
9. In your Discourse instance, navigate to the admin panel. Then open Site settings and then "Login" section.
10. Install [Discourse basic OAuth2 plugin](https://github.com/discourse/discourse-oauth2-basic) see plugin installation guide [here](https://github.com/deltachat-bot/deltachat-loginbot/pull/16/files).
11. Configure Basic OAuth2:
```
oauth2 enabled: 			true
oauth2 client id: 			secret
oauth2 client secret: 			secret
oauth2 authorize url:			https://login.testrun.org/authorize
oauth2 token url:			https://login.testrun.org/token
oauth2 token url method:		POST
oauth2 callback user id path:		params.info.userid
oauth2 callback user info paths:	name:params.info.username
					email:params.info.email
oauth2 fetch user details:		false
oauth2 email verified:			true
oauth2 button title:			Login with Delta Chat
oauth2 allow association change:	true
```
Enter the same client id and secret which you entered in `config.toml`. Change oauth2 authorize and token url according to your domain and where loginbot web API is listening.
18. There are other stuff you can configure according to your need. You should look up Discourse docs for this. For example, `oauth2 button title` is the title of button the user sees. Like you can enter "Login with DeltaChat". You can read more in [Discourse Basic OAuth2 support](https://meta.discourse.org/t/discourse-oauth2-basic-support/33879)

This photo shows an example configuration:


![Discourse example configuration](./discourse.png)
