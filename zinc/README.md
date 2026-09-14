# zinc

A zsh-compatible shell, written in Rust. The name is *zinc*, a metal beside
ferrous, and the Z of zsh. The acceptance criterion is that oh-my-zsh runs in
it.

It lives in the Ferrix tree but stands alone, as ferrousli does: its own cargo
workspace, a std Linux program whose one dependency is `libc`, reaching the
kernel only through Linux system calls. zsh 5.9 is the reference, and each
module names the part of zsh's source it follows.

## Building

For Ferrix, from this directory, on any host:

```
cargo build --release --target x86_64-unknown-linux-musl
```

`.cargo/config.toml` links it statically with rust-lld against the target's
own musl, so no C toolchain is needed. `cargo xtask build` and `run` do this
and put the result in the initramfs at `/bin/zinc` and `/bin/zsh`.

On Linux, `cargo build` and `cargo test` give a host binary to compare with
zsh. `zinc --tokens FILE` and `zinc --ast FILE` print the lexer's tokens and
the parser's trees.

## Where it stands

* **Lexer and parser:** zsh's `lex.c` and `parse.c`, in zsh's in-band token
  representation: every command form, here-documents, aliases, `[[ ]]`.
* **Execution:** lists, pipelines (the last element in the shell, as zsh
  does), redirections including here-documents and `{fd}>`, subshells,
  functions with `local`, `&`, command substitution, `if`/`for`/`while`/
  `until`/`repeat`/`case` with `;&` and `;|`, `{ } always { }`.
* **Expansion:** `$name`, `${...}` with the common flags (`j s f z U L C o O u
  k v q Q P @`), subscripts and `(r)`/`(I)` subscript flags, `:-`-family
  operators, `#`/`%`/`/` pattern operators, substrings, `:h :t :r :e :l :u :a
  :gs`, arithmetic, `$'...'`, brace expansion, `~`, globbing with `(N)`.
* **Builtins:** the ones scripts use first, from `echo`, `print` and `printf`
  to `typeset`, `read`, `source`, `autoload`, `getopts` and `trap EXIT`.
* **Interactive:** a prompt with the plain `%` escapes and continuation lines.
  No line editor, completion, job control or history yet.

Next, in the order oh-my-zsh needs them: the parser parity run against
`zsh -n` over oh-my-zsh and zsh's function library; the rest of the
expansion flags and glob qualifiers; `zstyle`, `zparseopts`, `zmodload` and
the special parameter hashes; prompt colours and `precmd` hooks; ZLE; the
completion system.
