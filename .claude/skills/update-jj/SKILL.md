---
name: update-jj
description: Updates the jj-lib and jj-cli dependencies. Use when asked to update the jj version.
---

<background-information>
Randori (this project) depends on multiple crates from the JJ project through its engine taken from GG. The engine is primarily an alternative to `jj-cli`, so it uses a lot of `jj-lib` code; however, it also uses `jj-cli` directly in a few cases
</background-information>

Updating crate versions
=======================
1. Modify Cargo.toml to update both `jj-lib` and `jj-cli` to the required version. This will either be a published version like `version = "0.37.0"` or a branch name like `git = "https://github.com/jj-vcs/jj.git", branch = "main"`. 
2. Run `cargo update`, which will update Cargo.lock.
3. Use /cargo-dedupe to match other dependencies' versions.

Fixing breaking changes
=======================
It's likely that jj API changes will require modifications to the engine code. Make any necessary modifications, maintaining the current behaviour. 
For examples of how to use modified jj APIs, consult the source code at https://github.com/jj-vcs/jj - the jj-cli code (`cli/` directory) often makes use of jj-lib code (`lib/` directory) in similar ways to the engine.

Verifying the update
==================
1. Run `cargo check` - there should be no warnings or errors.
2. Run `cargo test` - all tests should pass. 
3. Run `cargo build -p randori` - the gpui frontend in crates/randori must still build against the changed message structs in src/messages.