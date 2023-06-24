# buckle

Buckle is a launcher for buck2 and other binaries. It manages what version of a binary is used on a per-project basis. It picks a good version downloads it from the official releases, and then passes command line arguments through to the managed buck2 binary.

TODO:
1. Allow bootstrap from source (pinned).
2. Warn on prelude mismatch 

## Installation

At this time, only installing through crates.io is supported.

Packaging for various distros and/or releases on GitHub are highly likely.

```
cargo install buckle
```

## Running buckle

In general you use buckle like you would the tool it is running for you.  Behind the scenes it downloads archives, from which it runs binaries.

Here are some example invocations.

Exercise the default buck2 config: `buckle`

Test some scripts that download and run tools using the installed buckle. NB, you can remove the .toml extension if you install the scripts, its just there to show they are valid toml files:
```shell
    ../examples/buck2.toml
    ../examples/bazel7.toml
```

Test a config from build:
```shell
    BUCKLE_CONFIG_FILE=examples/bazel7.toml cargo run -- version
```

Test a config using the installed buckle:
```shell
    BUCKLE_CONFIG_FILE=examples/bazel7.toml buckle version
```

## Environment variables

`BUCKLE_CONFIG_FILE` points to a file to load config from

`BUCKLE_CONFIG` is an environment variable that can hold config. Mostly useful for testing.

`BUCKLE_SCRIPT` is used to tell buckle its being invoked as a script.  We use an env var for this as all command line arguments need to be passed to the underling tool.

`BUCKLE_BINARY` tells buckle which binary to run if there are multiple in the config

## Config syntax

`buckle` config is in tsoml, and allows you to specify which archives to download and cache and which binaries to run from those cached archives

The config file helps buckle find the archives to download and unpack, and from them which binaries to run

If there is no config, buckle with run buck2 with default config pointing to buck2 latest from github

For example config and how to use buckle as a #! interpreter see [examples](./examples/)

### Patterns for archives

When naming the archive you are looking for you can specify using a simple templating syntax.

`%version%`:  the artifact version name;

`%target%`: buckles view of the host's [rust target triple](https://rust-lang.github.io/rfcs/0131-target-specification.html);

`%arch%`: the architecture part of the triple

`%os%`: the os part of the triple

## What buck2 version does buckle use?

It depends on your config, but until meta tag github releases the only option that works is latest


