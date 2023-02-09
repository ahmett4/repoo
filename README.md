# Mina Indexer

The Mina indexer is an improvement and simplification over the
software that is collectively called "[archive
node](https://github.com/MinaProtocol/mina/tree/develop/src/app/archive)"
in the Mina codebase.

The Mina indexer replaces the archive node trio of architectural
elements (PostgreSQL database, Mina daemon, 'mina-archiver' process)
with a system that consumes the precomputed blocks by the Mina daemon
to generate an index for which we can efficiently query.

# Getting Started

This repository uses Nix Flakes as a development environment and build system. You can install Nix [here](https://nixos.org/download.html) Sand you can visit [this page](https://nixos.wiki/wiki/Flakes) for instructions on enabling Nix Flakes on your system. Apart from Nix, there are no external dependencies for this project!

# Building the Project

Binaries for `mina-indexer` can be built by running `nix build` with Flakes enabled (see above). All binaries are output to `./result/bin`

# Entering a Development Environment

You can enter a development environment by running `nix develop` at the command line. The development environment for this project takes care of installing all dependencies, compilers, and development tools (this means that you don't even need rustup installed!), including the `rust-analyzer` language server. For VSCode, we recommend the `Nix Environment Selector` extension pointed at `shell.nix` to tell your IDE about the installed tools, though you can also use direnv for this same purpose.

# License (See LICENSE file for full license)

Copyright 2023 Granola Systems Inc.

Free use of this software is granted under the terms of the Mozilla
Public License 2.0.