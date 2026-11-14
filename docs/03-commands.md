# Commands

This chapter describes all the available commands.

* [git reflow plan ↗](./04-plan.md)
* [git reflow pr ↗](./05-pr.md)
* [git reflow release ↗](./06-release.md)

```
$ git reflow help

A tool for automating semantic releases by raising pull-requests when commits enter release branches

Usage: git reflow [OPTIONS] <COMMAND>

Commands:
  plan     Plan the releases for packages/s without actually releasing anything
  pr       Create or update release pull request/s for changes to package/s
  release  Tags and releases the most recently merged release pull request
  config   Print the configuration and exit
  help     Print this message or the help of the given subcommand(s)

Options:
  -c, --config <PATH>  The config file path
  -v, --verbose...     Increase logging verbosity
  -q, --quiet...       Decrease logging verbosity
      --color <COLOR>  Control when to use colour [default: auto] [possible values: auto, always, never]
  -h, --help           Print help
  -V, --version        Print version
```
