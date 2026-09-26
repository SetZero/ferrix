//! What oh-my-zsh needs of the shell, checked against what zsh 5.9 does.
//!
//! Every case here came from running oh-my-zsh's installer or its start-up
//! under zinc and finding the place it stopped. The expected values are what
//! `zsh -c` prints on the machine this was written on; they are written out
//! rather than measured at test time, so the test says the same thing on a
//! machine with no zsh installed.

use std::process::Command;

/// What `zinc -c script` printed on standard output.
fn run(script: &str) -> String {
    match Command::new(env!("CARGO_BIN_EXE_zinc"))
        .args(["-c", script])
        .output()
    {
        Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
        Err(error) => format!("zinc did not start: {error}"),
    }
}

/// `${=spec}` splits at `$IFS` whether or not it is quoted, and an IFS
/// character that is not whitespace keeps the empty fields around it.
#[test]
fn a_forced_split_follows_the_ifs_rules() {
    assert_eq!(run("v='a b c'; print -l ${=v}"), "a\nb\nc\n");
    assert_eq!(run("v='a b c'; print -l \"${=v}\""), "a\nb\nc\n");
    assert_eq!(run("IFS=.; v='a..b'; print -l ${=v}"), "a\n\nb\n");
    assert_eq!(run("IFS=.; v='.a.'; a=(${=v}); echo ${#a}"), "3\n");
    assert_eq!(run("IFS=' .'; v='a . b'; a=(${=v}); echo ${#a}"), "2\n");
    assert_eq!(run("v=''; a=(${=v}); echo ${#a}"), "0\n");
    assert_eq!(run("IFS=; v='a b'; a=(${=v}); echo ${#a}"), "1\n");
    // The same rule for what a command substitution splits.
    assert_eq!(run("IFS=.; a=($(echo 'a..b')); echo ${#a}"), "3\n");
    assert_eq!(run("a=($(echo '  x  y ')); echo ${#a}"), "2\n");
}

/// Arithmetic reaches into arrays and hashes. `is-at-least`, which
/// oh-my-zsh asks about every version it cares about, is written in it.
#[test]
fn arithmetic_subscripts_a_parameter() {
    assert_eq!(run("a=(5 7); echo $(( a[1] + a[2] ))"), "12\n");
    assert_eq!(run("a=(5 7 9); i=1; echo $(( a[i+1] ))"), "7\n");
    assert_eq!(run("a=(5 7); echo $(( a[-1] )) $(( a[5] ))"), "7 0\n");
    // A hash takes its subscript literally, so `h[k]` is the key `k`.
    assert_eq!(run("typeset -A h; h[x]=4; k=x; echo $(( h[k] ))"), "0\n");
    assert_eq!(run("typeset -A h; h[x]=4; k=x; echo $(( h[$k] ))"), "4\n");
    // A scalar's subscript is one character of it.
    assert_eq!(run("s=59; echo $(( s[1] )) $(( s[2] ))"), "5 9\n");
    // And they are lvalues.
    assert_eq!(run("a=(5); (( a[1] += 2 )); echo $a[1]"), "7\n");
    assert_eq!(run("a=(5); (( ++a[1] )); echo $a[1]"), "6\n");
    assert_eq!(
        run("typeset -A h; h[k]=1; (( h[k] = 8 )); echo $h[k]"),
        "8\n"
    );
}

/// A hash reads the subscript flags differently from an array: zsh's
/// `colors` builds `$fg` out of `${color[(I)fg-*]}`, every matching key.
#[test]
fn subscript_flags_on_a_hash_answer_with_keys_and_values() {
    const HASH: &str = "typeset -A c; c=(fg-red 31 fg-blue 34 bg-red 41); ";
    assert_eq!(
        run(&format!("{HASH}echo ${{c[(I)fg-*]}}")),
        "fg-red fg-blue\n"
    );
    assert_eq!(run(&format!("{HASH}echo ${{c[(i)fg-*]}}")), "fg-red\n");
    assert_eq!(run(&format!("{HASH}echo ${{c[(r)3*]}}")), "31\n");
    assert_eq!(run(&format!("{HASH}echo ${{c[(R)3*]}}")), "31 34\n");
    assert_eq!(run(&format!("{HASH}echo ${{c[(k)fg-red]}}")), "31\n");
    // An array still answers with indices, the last one for `I`.
    assert_eq!(run("a=(x y z y); echo ${a[(I)y]} ${a[(i)y]}"), "4 2\n");
    assert_eq!(run("a=(x y); echo ${a[(I)q]}"), "0\n");
}

/// The prompt has two names and is one parameter, which is how a theme that
/// sets `PROMPT` changes the prompt the shell prints.
#[test]
fn the_prompt_parameters_share_one_value() {
    assert_eq!(run("PROMPT='x> '; echo \"[$PS1]\""), "[x> ]\n");
    assert_eq!(
        run("PS1='y> '; echo \"[$PROMPT][$prompt]\""),
        "[y> ][y> ]\n"
    );
    assert_eq!(run("PROMPT2='c> '; echo \"[$PS2]\""), "[c> ]\n");
    // The right-hand prompts are two parameters in zsh, not one.
    assert_eq!(run("RPROMPT=r; echo \"[$RPS1]\""), "[]\n");
}

/// The colour and attribute escapes, as the codes zsh's terminal
/// capabilities produce for a 256-colour terminal. Agnoster is built out of
/// these and nothing else.
#[test]
fn prompt_colours_are_the_sequences_zsh_writes() {
    let p = |spec: &str| run(&format!("print -Pn -- '{spec}'"));
    assert_eq!(p("%F{blue}"), "\x1b[34m");
    assert_eq!(p("%F{black}"), "\x1b[30m");
    assert_eq!(p("%K{blue}"), "\x1b[44m");
    // 0-7 are the plain codes, 8-15 the bright ones, the rest are indexed.
    assert_eq!(p("%F{7}"), "\x1b[37m");
    assert_eq!(p("%F{9}"), "\x1b[91m");
    assert_eq!(p("%F{200}"), "\x1b[38;5;200m");
    assert_eq!(p("%K{200}"), "\x1b[48;5;200m");
    // A name the terminal would not know is the default colour, not an error.
    assert_eq!(p("%F{default}"), "\x1b[39m");
    assert_eq!(p("%F{bogus}"), "\x1b[39m");
    assert_eq!(p("%f%k"), "\x1b[39m\x1b[49m");
    // There is no "not bold", so `%b` resets everything, as zsh's does.
    assert_eq!(p("%B%b"), "\x1b[1m\x1b[0m");
    assert_eq!(p("%U%u%S%s"), "\x1b[4m\x1b[24m\x1b[7m\x1b[27m");
    // `%{...%}` are markers; what they enclose is literal.
    assert_eq!(p("%{raw%}"), "raw");
    assert_eq!(p("%%"), "%");
}

/// `%(c.true.false)`, whose branches are themselves prompts.
#[test]
fn a_prompt_ternary_picks_a_branch() {
    let p = |spec: &str| run(&format!("print -Pn -- '{spec}'"));
    // The tests run unprivileged, so `!` is false and `?` starts true.
    assert_eq!(p("%(!.root.user)"), "user");
    assert_eq!(p("%(?.ok.bad)"), "ok");
    assert_eq!(p("%(100j.busy.idle)"), "idle");
    // The chosen branch is expanded in its turn.
    assert_eq!(p("%(?.%F{green}ok%f.no)"), "\x1b[32mok\x1b[39m");
    // Text after the ternary is not swallowed by it.
    assert_eq!(p("%(!.a.b)c"), "bc");
}

/// `$commands` is every program on `$PATH` by name, which is how a script
/// asks whether one is installed. oh-my-zsh asks constantly.
#[test]
fn commands_says_which_programs_exist() {
    assert_eq!(run("echo $+commands[sh]"), "1\n");
    assert_eq!(run("echo $+commands[definitely-not-here-xyzzy]"), "0\n");
    assert_eq!(run("[[ -x $commands[sh] ]] && echo yes"), "yes\n");
}

/// `<(cmd)` and `>(cmd)`: the word is the name of a pipe the command runs
/// on the other end of. oh-my-zsh's own lib reaches `source <(dircolors -b)`
/// before any theme is loaded.
#[test]
fn process_substitution_names_a_pipe() {
    assert_eq!(run("cat <(echo hi)"), "hi\n");
    assert_eq!(run("source <(echo x=5); echo $x"), "5\n");
    assert_eq!(run("cat <(echo a) <(echo b)"), "a\nb\n");
    // The parentheses inside belong to the command, not to the construct.
    assert_eq!(run("cat <(echo '(x)')"), "(x)\n");
    assert_eq!(run("cat <((echo nested))"), "nested\n");
    // As a redirection target, either way round.
    assert_eq!(run("wc -l < <(printf 'a\\nb\\n')"), "2\n");
    // The name is a word, so it can stand in the middle of one.
    assert!(run("echo A<(echo b)C").starts_with("A/proc/self/fd/"));
    // `<` also begins a numeric glob, which is a pattern and not a command.
    assert_eq!(run("[[ 5 = <-> ]] && echo num"), "num\n");
    assert_eq!(run("[[ 12 = <1-20> ]] && echo in"), "in\n");
    assert_eq!(run("case 7 in <->) echo digits;; esac"), "digits\n");
}

/// A shell that keeps its end of the pipe open must also let go of it, or a
/// prompt that substitutes on every draw runs the shell out of descriptors.
#[test]
fn process_substitution_closes_its_descriptors() {
    assert_eq!(
        run("for i in $(seq 1 300); do cat <(echo x) >/dev/null; done; echo done"),
        "done\n"
    );
}

/// A multibyte character survives being quoted. The shell escapes some
/// bytes internally, and escaping them twice is what turned agnoster's and
/// avit's glyphs into nonsense.
#[test]
fn a_multibyte_character_survives_quoting() {
    // U+25B6, whose middle byte is one the shell uses for a token of its own.
    assert_eq!(run("echo '\u{25b6}'"), "\u{25b6}\n");
    assert_eq!(run("echo \"\u{25b6}\""), "\u{25b6}\n");
    assert_eq!(run("echo \u{25b6}"), "\u{25b6}\n");
    assert_eq!(run("v='\u{25b6}'; echo $v"), "\u{25b6}\n");
    assert_eq!(run("echo $'\\u25b6'"), "\u{25b6}\n");
    assert_eq!(run("echo $'\\xe2\\x96\\xb6'"), "\u{25b6}\n");
    // Quoting still keeps a glob character literal.
    assert_eq!(run("echo '*'"), "*\n");
}

/// `%n` and `$USERNAME` are the password database's answer, not `$USER`:
/// agnoster asks `$USERNAME` whether the user is worth naming at all.
#[test]
fn the_user_is_named_by_the_password_database() {
    let who = run("print -Pn -- '%n'");
    assert!(!who.is_empty(), "%n gave nothing");
    assert_eq!(run("echo $USERNAME"), format!("{who}\n"));
    // A bogus $USER does not change either of them.
    assert_eq!(run("USER=nobody; print -Pn -- '%n'"), who);
    // LOGNAME comes from the same place, so a theme comparing the two --
    // avit does -- sees no difference that is only a missing value.
    assert_eq!(run("echo $LOGNAME"), format!("{who}\n"));
    assert_eq!(run("[[ $LOGNAME == $USERNAME ]] && echo same"), "same\n");
}

/// An escape that means nothing expands to nothing, rather than to itself.
/// The cloud theme's prompt holds a bare `%` with a space after it.
#[test]
fn an_unknown_prompt_escape_expands_to_nothing() {
    assert_eq!(run("print -Pn -- 'a% b'"), "ab");
    assert_eq!(run("print -Pn -- 'a%zb'"), "ab");
    assert_eq!(run("print -Pn -- 'a%'"), "a");
}

/// A `${v/pat/repl}` whose pattern holds an escaped delimiter. agnoster
/// turns a ref into a branch name with `${ref/refs\/heads\//…}`.
#[test]
fn a_replacement_pattern_may_escape_its_delimiter() {
    assert_eq!(
        run("r=refs/heads/master; echo \"${r/refs\\/heads\\//B }\""),
        "B master\n"
    );
    assert_eq!(run("r=a/b/c; echo \"${r/\\//-}\""), "a-b/c\n");
    assert_eq!(run("r=abc; echo \"${r/b/X}\""), "aXc\n");
}

/// `zstyle` keeps a real database: the most specific pattern that matches the
/// context answers. A `zstyle` that answered everything made `vcs_info`
/// believe its debug styles were set and print its whole trace into the
/// middle of agnoster's prompt.
#[test]
fn a_style_is_answered_by_the_most_specific_pattern() {
    let db = "zstyle ':a:*' col A1 A2; zstyle '*:b' col B1; ";
    assert_eq!(
        run(&format!("{db}zstyle -s ':a:b' col v; echo $v")),
        "A1 A2\n"
    );
    assert_eq!(
        run(&format!("{db}zstyle -s ':a:b' col v -; echo $v")),
        "A1-A2\n"
    );
    assert_eq!(
        run(&format!("{db}zstyle -a ':a:b' col a; echo ${{#a}}")),
        "2\n"
    );
    // Weight, not the order they were written in: `*` is worth 2, `:` 3 and
    // anything else 4, so the pattern that spells more out comes first.
    let both = "zstyle ':*:b' k P1; zstyle ':a*' k P2; ";
    assert_eq!(run(&format!("{both}zstyle -s ':a:b' k v; echo $v")), "P1\n");
    // `-t` is false for an undefined style and `-T` is true, which is the
    // difference vcs_info leans on.
    assert_eq!(
        run("zstyle ':x:*' f yes; zstyle -t ':x:1' f; echo $?"),
        "0\n"
    );
    assert_eq!(
        run("zstyle ':x:*' f no; zstyle -t ':x:1' f; echo $?"),
        "1\n"
    );
    assert_eq!(run("zstyle -t ':none:1' f; echo $?"), "2\n");
    assert_eq!(run("zstyle -T ':none:1' f; echo $?"), "0\n");
    assert_eq!(
        run("zstyle ':x:*' f yes; zstyle -b ':x:1' f v; echo $v"),
        "yes\n"
    );
    assert_eq!(run("zstyle -b ':none:1' f v; echo $v"), "no\n");
    // `-e` is code, run at each lookup, whose `reply` is the value.
    assert_eq!(
        run("zstyle -e ':e:*' k 'reply=(EV)'; zstyle -s ':e:1' k v; echo $v"),
        "EV\n"
    );
    // `-d` forgets, and `-L` prints definitions back as commands.
    assert_eq!(run("zstyle ':p:*' k v1; zstyle -L"), "zstyle ':p:*' k v1\n");
    assert_eq!(run("zstyle ':p:*' k v1; zstyle -d; zstyle -L"), "");
}

/// `$functions` names every function the shell knows, defined or only marked
/// by `autoload`. vcs_info asks `(( $+functions[VCS_INFO_detect_git] ))`
/// before it will use a backend.
#[test]
fn functions_says_which_functions_exist() {
    assert_eq!(run("f() { :; }; echo ${+functions[f]}"), "1\n");
    assert_eq!(run("echo ${+functions[nope]}"), "0\n");
    assert_eq!(run("f() { :; }; v=f; echo ${+functions[$v]}"), "1\n");
    // A name marked but never found on `fpath` answers as zsh's does.
    assert_eq!(
        run("fpath=(); autoload -Uz nowhere; echo $functions[nowhere]"),
        "builtin autoload -XU\n"
    );
}

/// EXTENDED_GLOB's `x~y`: what `x` matches, less what `y` does. vcs_info
/// finds its backends with `${^fpath}/VCS_INFO_get_data_*~*(\~|.zwc)(N)`,
/// which without the operator either finds everything or nothing.
#[test]
fn the_except_operator_takes_matches_away() {
    let e = "setopt extendedglob; ";
    assert_eq!(run(&format!("{e}[[ abc == *~*x ]] && echo yes")), "yes\n");
    assert_eq!(run(&format!("{e}[[ abc == *~abc ]] || echo no")), "no\n");
    assert_eq!(
        run(&format!("{e}[[ abc == *~(x|y) ]] && echo yes")),
        "yes\n"
    );
    // `|` binds looser than `~`, so this is `(a*~ab)` or `ac`. The bar is
    // written inside a group because a bare one ends the condition, in zsh
    // as here.
    assert_eq!(
        run(&format!("{e}[[ ac == (a*~ab|ac) ]] && echo yes")),
        "yes\n"
    );
    assert_eq!(
        run(&format!("{e}[[ ad == (a*~ab|ac) ]] && echo yes")),
        "yes\n"
    );
    assert_eq!(
        run(&format!("{e}[[ ab == (a*~ab|ac) ]] || echo no")),
        "no\n"
    );
    // The backup and compiled files vcs_info's own glob leaves out.
    let p = "*~*(\\~|.zwc)";
    assert_eq!(
        run(&format!("{e}[[ get_data_git == {p} ]] && echo yes")),
        "yes\n"
    );
    assert_eq!(
        run(&format!("{e}[[ get_data_git.zwc == {p} ]] || echo no")),
        "no\n"
    );
    // A `~` with nothing after it is the character, and a word with no
    // wildcard beside it is not a glob at all.
    assert_eq!(run(&format!("{e}[[ f~ == *~ ]] && echo yes")), "yes\n");
    assert_eq!(run(&format!("{e}echo a~b")), "a~b\n");
}

/// `(#b)` remembers where each group matched, in `$match` and the two arrays
/// of offsets. vcs_info names its backends with it -- `${file:#(#b)…_(*)}` --
/// and its git backend reads a ref apart the same way.
#[test]
fn a_pattern_can_remember_its_groups() {
    let e = "setopt extendedglob; ";
    assert_eq!(
        run(&format!(
            "{e}f=VCS_INFO_get_data_git; : ${{f:#(#b)VCS_INFO_get_data_(*)}}; echo $match $mbegin $mend"
        )),
        "git 19 21\n"
    );
    assert_eq!(
        run(&format!("{e}[[ abcdef == (#b)a(b*)(e)f ]] && echo $match")),
        "bcd e\n"
    );
    assert_eq!(
        run(&format!(
            "{e}[[ abcdef == (#b)a(b*)(e)f ]] && echo $mbegin $mend"
        )),
        "2 5 4 5\n"
    );
    // A group takes as much as it can, which is the path zsh reports.
    assert_eq!(
        run(&format!("{e}[[ aXbXc == (#b)(*)X* ]] && echo $match")),
        "aXb\n"
    );
    // Without the flag nothing is written down.
    assert_eq!(run(&format!("{e}[[ ab == (a)(b) ]] && echo ok")), "ok\n");
}

/// A brace range expands its endpoints first: zsh runs parameter expansion
/// before brace expansion, the opposite of the other shells. `vcs_info`
/// walks its messages with `{1..${#msgs}}`, and compaudit writes the same
/// thing as `{1..$#_i_addfiles}`.
#[test]
fn a_brace_range_counts_a_parameter_in_its_endpoints() {
    assert_eq!(
        run("a=(x y z); echo {1..${#a}}"),
        "1 2 3
"
    );
    assert_eq!(
        run("a=(x y z); echo {1..$#a}"),
        "1 2 3
"
    );
    assert_eq!(
        run("n=4; echo {1..$n}"),
        "1 2 3 4
"
    );
    assert_eq!(
        run("s=abcd; echo {1..$#s}"),
        "1 2 3 4
"
    );
    // A step is an endpoint too, and a literal range is what it always was.
    assert_eq!(
        run("n=3; echo {1..10..$n}"),
        "1 4 7 10
"
    );
    assert_eq!(
        run("echo {a..c}"),
        "a b c
"
    );
    // Nothing to count is nothing to expand: the range stays as written.
    assert_eq!(
        run("a=(); echo {1..${#a}}"),
        "1 0
"
    );
}

/// The path escapes take a count of components, and all but `%C` write a
/// leading `$HOME` as `~`. robbyrussell, the theme Ferrix boots with, is
/// `%c`, which is one trailing component of the contracted path -- in a home
/// directory, `~` rather than the directory's name.
#[test]
fn a_path_escape_counts_components_of_the_contracted_path() {
    // A directory of its own, two levels under the home, so that both ends
    // of the path are visible. Made by the shell itself, because `cd` is
    // what moves it: assigning `$PWD` moves zsh not at all.
    let home = "/tmp/zinc-prompt-escapes";
    let deep = format!("mkdir -p {home}/oh-my-zsh/lib; HOME={home}; cd {home}/oh-my-zsh/lib; ");
    let show = |spec: &str| format!("{deep}print -P -- {spec}");
    assert_eq!(run(&show("%d")), format!("{home}/oh-my-zsh/lib\n"));
    assert_eq!(run(&show("%1d")), "lib\n");
    assert_eq!(run(&show("%2d")), "oh-my-zsh/lib\n");
    // A count larger than the path is the whole path, and a negative one
    // counts from the front, keeping the root.
    assert_eq!(run(&show("%9d")), format!("{home}/oh-my-zsh/lib\n"));
    assert_eq!(run(&show("%-1d")), "/tmp\n");
    assert_eq!(run(&show("%~")), "~/oh-my-zsh/lib\n");
    assert_eq!(run(&show("%1~")), "lib\n");
    assert_eq!(run(&show("%-1~")), "~\n");
    // `%c` is `%~` with one component asked for by default; `%C` is `%c`
    // without the contraction.
    assert_eq!(run(&show("%c")), "lib\n");
    assert_eq!(run(&show("%2c")), "oh-my-zsh/lib\n");
    assert_eq!(run(&show("%9c")), "~/oh-my-zsh/lib\n");
    // In the home directory itself the contraction is the whole answer.
    let at_home = format!("mkdir -p {home}; HOME={home}; cd {home}; print -P -- ");
    assert_eq!(run(&format!("{at_home}%c")), "~\n");
    assert_eq!(run(&format!("{at_home}%C")), "zinc-prompt-escapes\n");
    // The root has one component whatever is asked for.
    assert_eq!(run("cd /; print -P -- %1c"), "/\n");
}

/// Every function call parses its own options: OPTIND starts at 1 in it and
/// the caller's is back afterwards. oh-my-zsh's `compdef _git gco=git-checkout`
/// and `add-zsh-hook` both `shift $(( OPTIND - 1 ))`, and once an earlier
/// function had taken an option they shifted their first argument away.
#[test]
fn each_function_call_has_its_own_optind() {
    const F: &str = "f() { while getopts ab o; do :; done; echo in $OPTIND; }; ";
    assert_eq!(
        run("f() { while getopts ab o; do :; done; }; \
             g() { while getopts ab o; do echo got $o; done; \
                   shift $(( OPTIND - 1 )); echo $1; }; \
             f -a -b; g _git x=y"),
        "_git\n"
    );
    assert_eq!(
        run(&format!(
            "{F}while getopts ab o -a x; do :; done; echo top $OPTIND; f -a -b y; echo top $OPTIND"
        )),
        "top 2\nin 3\ntop 2\n"
    );
    // POSIX_BUILTINS shares one OPTIND between the caller and the call.
    assert_eq!(
        run("setopt posixbuiltins; f() { while getopts ab o; do :; done; }; f -a y; echo $OPTIND"),
        "2\n"
    );
}

/// Flags open with a plain `(` inside double quotes, where the lexer leaves
/// it untokenized. compinit reads each `#compdef -k` header's keys out of
/// `"${(@)_i_line[2,-1]}"`, and oh-my-zsh quotes flags throughout.
#[test]
fn flags_apply_inside_double_quotes() {
    const A: &str = "a=(one two three); f() { echo \"$#:$*\"; }; ";
    assert_eq!(
        run(&format!("{A}echo \"[${{(j:,:)a}}]\"")),
        "[one,two,three]\n"
    );
    assert_eq!(
        run(&format!("{A}echo \"[${{(U)a}}]\"")),
        "[ONE TWO THREE]\n"
    );
    assert_eq!(run(&format!("{A}f \"${{(@)a}}\"")), "3:one two three\n");
    assert_eq!(run(&format!("{A}f \"${{(@)a[2,-1]}}\"")), "2:two three\n");
    assert_eq!(run(&format!("{A}e=(); f \"${{(@)e}}\"")), "0:\n");
    assert_eq!(run(&format!("{A}f \"${{(@M)a:#t*}}\"")), "2:two three\n");
    assert_eq!(
        run(&format!("{A}x=\"${{(j:-:)a}}\"; echo $x")),
        "one-two-three\n"
    );
}

/// `autoload` marks a name and the first call reads its file; `+X` reads it
/// at once and fails when there is none. compinit marks every completion
/// function there is, so reading each one when it is marked made every
/// start-up parse some 1200 files.
#[test]
fn autoload_reads_at_the_first_call_or_at_plus_x() {
    let dir = "/tmp/zinc-autoload-parity";
    let setup = format!(
        "rm -rf {dir}; mkdir -p {dir}; \
         print -r -- 'echo \"hi from lazy $1\"' > {dir}/lazy; \
         print -r -- 'echo \"hi from eager\"' > {dir}/eager; \
         fpath=({dir} $fpath); "
    );
    assert_eq!(
        run(&format!(
            "{setup}autoload -Uz lazy; echo marked ${{+functions[lazy]}}; lazy one"
        )),
        "marked 1\nhi from lazy one\n"
    );
    assert_eq!(
        run(&format!(
            "{setup}autoload -Uz +X eager && echo \"+X ok ${{+functions[eager]}}\"; \
             autoload -Uz +X nosuch 2>/dev/null || echo '+X missing fails'"
        )),
        "+X ok 1\n+X missing fails\n"
    );
}

/// A `case` inside a quoted command substitution: its `(pattern)` items
/// close their own parentheses, and `esac)` ends the substitution. zsh's
/// `_ant` builds its targets that way, and compinit loads it.
#[test]
fn a_case_in_a_quoted_command_substitution_closes() {
    assert_eq!(run("x=\"$(case a in (a) echo A ;; esac)\"; echo $x"), "A\n");
    assert_eq!(
        run("x=\"${$(case a in a) echo B ;; esac)}\"; echo $x"),
        "B\n"
    );
    assert_eq!(
        run("x=\"$(case a in (a) (echo C) ;; esac)\"; echo $x"),
        "C\n"
    );
    assert_eq!(
        run("x=\"$(case a in (b) echo no ;; (*) echo D ;; esac; echo E)\"; echo $x"),
        "D\nE\n"
    );
}

/// `$(< file)` is the file's contents. oh-my-zsh reads its completion dump
/// that way to see whether the dump is still its own, and threw it away at
/// every start while this answered nothing.
#[test]
fn a_lone_input_redirection_substitutes_the_file() {
    const F: &str = "f=/tmp/zinc-parity-read-file; printf 'a\nb\n' > $f; ";
    assert_eq!(run(&format!("{F}echo \"[$(<$f)]\"")), "[a\nb]\n");
    assert_eq!(run(&format!("{F}echo \"[$( < $f )]\"")), "[a\nb]\n");
    assert_eq!(
        run(&format!("{F}a=(\"${{(@f)$(<\"$f\")}}\"); echo ${{#a}}")),
        "2\n"
    );
    assert_eq!(
        run("x=$(< /tmp/zinc-parity-no-such-file); echo \"st=$? [$x]\""),
        "st=1 []\n"
    );
}

/// An element of an array or a hash is read and set where the parameter is
/// kept. compinit reads and writes `_comps`, a hash of every command it can
/// complete, one key at a time; while each of those copied the whole hash,
/// a cold start of oh-my-zsh spent most of its time copying.
#[test]
fn an_element_is_read_and_set_in_place() {
    assert_eq!(
        run("a=(x y z); a[2]=Y; a[5]=E; echo ${a[2]} ${#a} \"[${a[4]}]\" $a[-1]"),
        "Y 5 [] E\n"
    );
    assert_eq!(
        run("typeset -A h; h[k]=1; h[k]+=2; h[j]=3; echo $h[k] ${h[j]} ${+h[n]} ${+h[j]}"),
        "12 3 0 1\n"
    );
    assert_eq!(run("a=(1); a+=2; a+=(3 4); echo $a ${#a}"), "1 2 3 4 4\n");
    assert_eq!(
        run("a=(p q r); i=2; echo ${a[i]} ${a[$i]} ${a[2,-1]} ${a[(i)r]} ${a[(r)q*]} ${a[(I)z]}"),
        "q q q r 3 q 0\n"
    );
    assert_eq!(
        run("typeset -A h; h=(one 1 two 2); echo ${h[(i)t*]} ${h[(r)1]} ${h[(k)one]}"),
        "two 1 1\n"
    );
    // One key of `$commands` is one look-up down `$PATH`, which a name with
    // a slash in it is never the key of.
    assert_eq!(
        run("echo ${+commands[sh]} ${+commands[no-such-program-here]} ${+commands[/bin/sh]}"),
        "1 0 0\n"
    );
}

/// `read` from a regular file takes one line and leaves the rest where the
/// next reader finds it, as reading a byte at a time would: it reads a block
/// and gives back what the line did not use.
#[test]
fn read_leaves_a_file_where_the_line_ended() {
    const F: &str = "f=/tmp/zinc-parity-read-offset; ";
    assert_eq!(
        run(&format!(
            "{F}print -l one two three > $f; {{ read a; read b; echo \"$a,$b\"; cat; }} < $f"
        )),
        "one,two\nthree\n"
    );
    // A line continued with a backslash is read on into the next.
    assert_eq!(
        run(&format!(
            "{F}print -rn -- 'a\\' > $f; print b >> $f; print c >> $f; \
             {{ read x; read -r y; echo \"$x|$y\"; }} < $f"
        )),
        "ab|c\n"
    );
}

/// `autoload` finds a burst of names -- compinit's, one per completion
/// function -- in one listing of `fpath`'s directories, and a file written
/// after an earlier listing is found all the same.
#[test]
fn autoload_finds_a_burst_of_names_and_a_file_written_since() {
    let dir = "/tmp/zinc-autoload-listing";
    assert_eq!(
        run(&format!(
            "rm -rf {dir}; mkdir -p {dir}; \
             for n in f1 f2 f3 f4 f5 f6; do print -r -- \"echo hi from $n\" > {dir}/$n; done; \
             fpath=({dir} $fpath); autoload -Uz f1 f2 f3 f4 f5 f6; f6; \
             print -r -- 'echo late' > {dir}/late; autoload -Uz f1 f2 f3 f4 f5 late; late"
        )),
        "hi from f6\nlate\n"
    );
}

/// The last command of a subshell or a substitution, when it is external,
/// runs in place of the process forked for it, as zsh runs it; what the
/// shell sees of it -- its status, its output, the signal that ended it --
/// is what it saw when a second process ran it.
#[test]
fn a_subshells_last_command_runs_in_its_place() {
    assert_eq!(
        run("(exit 3); echo $?; x=$(sh -c 'exit 4'); echo $?"),
        "3\n4\n"
    );
    assert_eq!(run("x=$(sh -c 'kill -TERM $$'); echo $?"), "143\n");
    assert_eq!(
        run("echo $(echo a; echo b) \"$(printf '%s-' 1 2)\""),
        "a b 1-2-\n"
    );
    assert_eq!(run("x=$(false; echo after); echo \"$x $?\""), "after 0\n");
    assert_eq!(run("f() { echo in-f; }; x=$(f); echo $x"), "in-f\n");
    // A pipeline's last element is still forked, so the elements before it
    // keep a parent that waits for them.
    assert_eq!(run("x=$(echo one two | wc -w); echo $x"), "2\n");
}
