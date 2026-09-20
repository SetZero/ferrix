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
  does, and each its own job, so a command written in a function or an `if`
  is waited for before the next one starts), redirections including here-documents and `{fd}>`, subshells,
  functions with `local`, `&`, command substitution, `if`/`for`/`while`/
  `until`/`repeat`/`case` with `;&` and `;|`, `{ } always { }`.
* **Expansion:** `$name`, `${...}` with the common flags (`j s f z U L C o O u
  k v q Q P @`), subscripts and `(r)`/`(I)` subscript flags, `:-`-family
  operators, `#`/`%`/`/` pattern operators, substrings, `:h :t :r :e :l :u :a
  :gs`, arithmetic, `$'...'`, brace expansion, `~`, globbing with `(N)`.
* **Builtins:** the ones scripts use first, from `echo`, `print` and `printf`
  to `typeset`, `read`, `source`, `autoload`, `getopts` and `trap EXIT`, and
  `umask` in both of zsh's forms -- octal, and a symbolic mode such as
  `umask g-w,o-w`, printed back by `umask -S`.
* **Interactive:** a prompt with the plain `%` escapes and continuation
  lines, the line editor with completion and history, and job control: a
  pipeline is one process group, the terminal is handed to the foreground
  job and taken back, and `jobs`, `fg`, `bg`, `wait`, `disown` and `kill %1`
  work over `%+`, `%-`, `%n`, `%name` and `%?text`. Ctrl-Z suspends, `fg`
  and `bg` resume, and `exit` with a job suspended is refused once. What is
  not there: no `SIGCHLD` handler, so a background job that finishes is
  reported at the next prompt rather than at once (zsh's `NOTIFY`), and a
  pipeline whose last element is a builtin runs that element in the shell,
  which is not in the job's group and so is not suspended with it.
* **A login shell:** `-l`, or a name beginning with `-`, as `login` and `su -`
  start one; `/etc/zshenv`, `.zshenv`, `/etc/zprofile`, `.zprofile`,
  `/etc/zshrc`, `.zshrc`, `/etc/zlogin` and `.zlogin` in zsh's order. It is
  the `ferrix` user's shell in Ferrix's image, so `su - ferrix` starts it.
* **Who is running it:** `UID`, `EUID`, `GID` and `EGID`, read from the
  kernel at each use, which is what `%#` and a theme's prompt ask.

**oh-my-zsh's installer runs.** `sh -c "$(curl -fsSL .../tools/install.sh)"`
clones the repository, writes a `.zshrc` from the template and prints the
banner, on this host and on Ferrix. What it needed was two fixes: `umask`
taking a symbolic mode, which is the installer's first line, and a pipeline
written inside a function, an `if`, a `while` or a `{ }` becoming a job of
its own rather than joining the job of the compound command around it --
without which `git init "$ZSH" && cd "$ZSH"` ran the `cd` while `git` was
still starting.

**oh-my-zsh starts, and Ferrix boots into it.** Sourcing it runs to the end
and leaves a themed prompt behind. The image carries a checkout at
`/usr/share/oh-my-zsh` and an `/etc/zshrc` that sources it, installed on the
build machine by `cargo xtask omz --from <DIRECTORY-OR-URL>`, so the shell
the kernel starts is an oh-my-zsh shell: `cargo xtask test-jobs` now types
its whole session at robbyrussell's prompt. What that took, in the order it
was found: `zcompile` and a digest to read back; prompt expansion with
`PROMPT_SUBST`, colours and ternaries; `$commands`; process substitution;
`zstyle` as a real database rather than a no-op, which is what made
`vcs_info` run at all; `$functions`; EXTENDED_GLOB's `~`; `(#b)`
backreferences; and the path escapes' component counts.

Not every theme is right yet. Of ten measured against zsh 5.9 by rendering
`$PROMPT` in the same home, `robbyrussell` -- the one the image boots with
-- `avit` and `simple` come out byte for byte. The other seven differ in
three ways: a bold attribute zinc writes where zsh writes none, the
256-colour escapes `spectrum.zsh` builds, and `vcs_info`, which reaches
`zformat` and stops.

Next, in the order the themes above need them: the bold attribute and the
256-colour escapes; `zformat` and `zparseopts`, which is where `vcs_info`
stops; the rest of the expansion flags and glob qualifiers; `zmodload` and
the special parameter hashes; the completion system proper.
