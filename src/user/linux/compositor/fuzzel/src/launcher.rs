//! The launcher's state and what each input does to it: fuzzel's
//! `execute_binding`, `execute_selected` and the tail of `keyboard_key`.
//!
//! No socket and no pixel: a key's action or text goes in, and out comes
//! whether to redraw and whether (and how) to finish. The window turns the
//! finish into a started program or a printed line.

use crate::config::{Action, Config, MatchMode};
use crate::desktop::Application;
use crate::matching::{Matcher, Matches};
use crate::prompt::Prompt;

/// How the launcher finishes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finish {
    /// Start (or in dmenu mode, print) the entry at this index into
    /// `apps`, or with `None` the typed text; then exit with `code` if it
    /// worked.
    Execute {
        /// The entry, or none for the typed text.
        app: Option<usize>,
        /// The exit status on success: 0, or 10 + N for `custom-N`.
        code: i32,
    },
    /// Exit with this status and start nothing.
    Cancel(i32),
}

/// What an input did.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Outcome {
    /// Whether the window should be drawn again.
    pub redraw: bool,
    /// Whether the launcher is done, and how.
    pub finish: Option<Finish>,
}

/// Everything the window shows and the keys change.
#[derive(Clone, Debug)]
pub struct Launcher {
    /// The entries.
    pub apps: Vec<Application>,
    /// The line being typed.
    pub prompt: Prompt,
    /// What matches it.
    pub matches: Matches,
    /// Whether the list is still hidden for `hide-before-typing`.
    pub hidden_until_typing: bool,
    /// dmenu mode.
    pub dmenu: bool,
    /// `--only-match`.
    pub only_match: bool,
    /// `auto-select`.
    pub auto_select: bool,
    /// Whether an entry's count changed in a way the cache must be written
    /// for even without a launch (`expunge`).
    pub force_cache_update: bool,
    /// Whether `expunge` or insertion changed what `mode` is.
    mode: MatchMode,
}

impl Launcher {
    /// A launcher over `apps`, as `config` says.
    #[must_use]
    pub fn new(config: &Config, apps: Vec<Application>) -> Self {
        let matcher = Matcher {
            fields: config.fields,
            mode: config.match_mode,
            fuzzy: config.fuzzy,
            sort: config.sort_result,
        };
        let mut launcher = Self {
            apps,
            prompt: Prompt::new(&config.prompt, &config.placeholder, &config.search_text),
            matches: Matches::new(matcher, usize::try_from(config.lines).unwrap_or(usize::MAX)),
            hidden_until_typing: config.hide_before_typing,
            dmenu: config.dmenu.enabled,
            only_match: config.dmenu.only_match,
            auto_select: config.auto_select,
            force_cache_update: false,
            mode: config.match_mode,
        };
        launcher.refilter();
        launcher
    }

    /// Match everything again, as a deletion does.
    pub fn refilter(&mut self) {
        let prompt = self.prompt.text.clone();
        self.matches.update(&self.apps, &prompt, false, true);
    }

    /// The selected entry, as an index into `apps`.
    #[must_use]
    pub fn selected(&self) -> Option<usize> {
        self.matches.selected_match().map(|m| m.app)
    }

    /// Whether the list is drawn.
    #[must_use]
    pub fn list_shown(&self) -> bool {
        !self.hidden_until_typing || !self.prompt.text.is_empty()
    }

    fn execute(&self, as_is: bool, code: i32) -> Option<Finish> {
        Some(Finish::Execute {
            app: if as_is { None } else { self.selected() },
            code,
        })
    }

    fn cancel(&self) -> Option<Finish> {
        Some(Finish::Cancel(if self.dmenu { 2 } else { 0 }))
    }

    /// `auto-select`: one match left is executed.
    fn auto(&self, redraw: bool) -> Option<Finish> {
        (redraw && self.auto_select && self.matches.list.len() == 1)
            .then(|| self.execute(false, 0))
            .flatten()
    }

    /// Text typed: inserted, matched incrementally (fully in fuzzy mode),
    /// the first match selected.
    pub fn type_text(&mut self, text: &str) -> Outcome {
        self.prompt.insert(text);
        if !self.prompt.text.is_empty() {
            self.hidden_until_typing = false;
        }
        let incremental = self.mode != MatchMode::Fuzzy;
        let prompt = self.prompt.text.clone();
        self.matches.update(&self.apps, &prompt, incremental, true);
        let _ = self.matches.select(0);
        Outcome {
            redraw: true,
            finish: self.auto(true),
        }
    }

    /// An edit that changes the text: matched again from the whole list.
    fn edited(&mut self, changed: bool) -> bool {
        if changed {
            self.refilter();
        }
        changed
    }

    /// A key binding's action.
    pub fn action(&mut self, action: Action) -> Outcome {
        let mut finish = None;
        let redraw = match action {
            Action::Cancel => {
                finish = self.cancel();
                false
            }
            Action::CursorHome => self.prompt.home(),
            Action::CursorEnd => self.prompt.end(),
            Action::CursorLeft => self.prompt.prev_char(),
            Action::CursorLeftWord => self.prompt.prev_word_move(),
            Action::CursorRight => self.prompt.next_char(),
            Action::CursorRightWord => self.prompt.next_word_move(),
            Action::DeleteLine => {
                let changed = self.prompt.erase_all();
                self.edited(changed)
            }
            Action::DeletePrev => {
                let changed = self.prompt.erase_prev_char();
                self.edited(changed)
            }
            Action::DeletePrevWord => {
                let changed = self.prompt.erase_prev_word();
                self.edited(changed)
            }
            Action::DeleteLineBackward => {
                let changed = self.prompt.erase_before_cursor();
                self.edited(changed)
            }
            Action::DeleteNext => {
                let changed = self.prompt.erase_next_char();
                self.edited(changed)
            }
            Action::DeleteNextWord => {
                let changed = self.prompt.erase_next_word();
                self.edited(changed)
            }
            Action::DeleteLineForward => {
                let changed = self.prompt.erase_after_cursor();
                self.edited(changed)
            }
            Action::InsertSelected => self.insert_selected(),
            Action::Expunge => match self.selected() {
                Some(at) => {
                    if let Some(app) = self.apps.get_mut(at) {
                        app.count = 0;
                    }
                    self.refilter();
                    self.force_cache_update = true;
                    true
                }
                None => false,
            },
            // The clipboards are the window's to read; what they give comes
            // back through `type_text`.
            Action::ClipboardPaste | Action::PrimaryPaste => false,
            Action::Execute => {
                let count = self.matches.list.len();
                if !(self.prompt.text.is_empty() && count == 0)
                    && !(self.dmenu && self.only_match && count == 0)
                {
                    finish = self.execute(false, 0);
                }
                false
            }
            Action::ExecuteOrNext => {
                if self.matches.list.len() == 1 {
                    finish = self.execute(false, 0);
                    false
                } else {
                    self.matches.next(true)
                }
            }
            Action::ExecuteInput => {
                finish = self.execute(true, 0);
                false
            }
            Action::Prev => self.matches.prev(false),
            Action::PrevWithWrap => self.matches.prev(true),
            Action::PrevPage => self.matches.prev_page(false),
            Action::Next => self.matches.next(false),
            Action::NextWithWrap => self.matches.next(true),
            Action::NextPage => self.matches.next_page(false),
            Action::First => self.matches.first(),
            Action::Last => self.matches.last(),
            Action::Custom(n) => {
                finish = self.execute(false, 9 + i32::from(n));
                false
            }
        };
        if finish.is_none() {
            finish = self.auto(redraw);
        }
        Outcome { redraw, finish }
    }

    /// `insert-selected`: the entry's `Exec` (in dmenu mode its text)
    /// replaces what was typed.
    fn insert_selected(&mut self) -> bool {
        let Some(app) = self.selected().and_then(|at| self.apps.get(at)) else {
            return false;
        };
        let text = if self.dmenu {
            Some(app.title_string())
        } else {
            app.exec.clone()
        };
        let _ = self.prompt.erase_all();
        let Some(text) = text else {
            return false;
        };
        self.prompt.insert(&text);
        self.refilter();
        true
    }

    /// The pointer over row `row` of the page selects it.
    pub fn hover(&mut self, row: usize) -> bool {
        let before = self.matches.selected;
        let _ = self.matches.select_on_page(row);
        before != self.matches.selected
    }

    /// A left click on row `row` executes it.
    pub fn click(&mut self, row: usize) -> Outcome {
        if self.matches.select_on_page(row) {
            return Outcome {
                redraw: true,
                finish: self.execute(false, 0),
            };
        }
        Outcome::default()
    }

    /// A right click, which is Escape.
    pub fn right_click(&mut self) -> Outcome {
        Outcome {
            redraw: false,
            finish: self.cancel(),
        }
    }

    /// The keyboard went to another surface, with `exit-on-keyboard-focus-loss`.
    #[must_use]
    pub fn focus_lost(&self) -> Outcome {
        Outcome {
            redraw: false,
            finish: Some(Finish::Cancel(if self.dmenu { 1 } else { 0 })),
        }
    }

    /// Count a successful start, for the cache.
    pub fn started(&mut self, app: Option<usize>) {
        if let Some(app) = app.and_then(|at| self.apps.get_mut(at)) {
            app.count = app.count.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Finish, Launcher};
    use crate::config::{Action, Config};
    use crate::desktop::{Application, lower};

    fn app(title: &str, exec: &str) -> Application {
        Application {
            id: Some(format!("{}.desktop", title.to_lowercase())),
            title: title.chars().collect(),
            title_lower: lower(title),
            basename: lower(title),
            exec: Some(exec.to_owned()),
            wexec: lower(exec),
            visible: true,
            ..Application::default()
        }
    }

    fn apps() -> Vec<Application> {
        vec![
            app("Chrome", "/bin/chrome"),
            app("Terminal", "/bin/term /bin/zinc"),
            app("Top", "/bin/top"),
        ]
    }

    #[test]
    fn type_part_of_a_name_and_press_enter() {
        let mut l = Launcher::new(&Config::defaults(1), apps());
        assert_eq!(l.matches.list.len(), 3);
        for c in ["t", "e", "r"] {
            let _ = l.type_text(c);
        }
        let first = l
            .selected()
            .and_then(|at| l.apps.get(at))
            .map(Application::title_string);
        assert_eq!(first.as_deref(), Some("Terminal"));
        let done = l.action(Action::Execute);
        assert_eq!(
            done.finish,
            Some(Finish::Execute {
                app: Some(1),
                code: 0
            })
        );
    }

    #[test]
    fn navigation_and_editing() {
        let mut l = Launcher::new(&Config::defaults(1), apps());
        assert!(l.action(Action::Next).redraw);
        assert_eq!(l.selected(), Some(1));
        let _ = l.type_text("xyz");
        assert!(l.matches.list.is_empty());
        // Enter with no match but text typed runs the text.
        assert_eq!(
            l.action(Action::Execute).finish,
            Some(Finish::Execute { app: None, code: 0 })
        );
        assert!(l.action(Action::DeleteLine).redraw);
        assert_eq!(l.matches.list.len(), 3);
        // Enter with nothing typed and nothing listed does nothing: here
        // there are entries, so it runs the first.
        assert!(l.action(Action::Execute).finish.is_some());
        assert_eq!(l.action(Action::Cancel).finish, Some(Finish::Cancel(0)));
        assert_eq!(
            l.action(Action::Custom(3)).finish,
            Some(Finish::Execute {
                app: l.selected(),
                code: 12
            })
        );
        let _ = l.type_text("to");
        assert!(l.action(Action::InsertSelected).redraw);
        assert_eq!(l.prompt.text_string(), "/bin/top");
    }

    #[test]
    fn tab_executes_a_single_match_and_auto_select() {
        let mut l = Launcher::new(&Config::defaults(1), apps());
        let _ = l.type_text("chr");
        assert_eq!(
            l.action(Action::ExecuteOrNext).finish,
            Some(Finish::Execute {
                app: Some(0),
                code: 0
            })
        );
        let mut config = Config::defaults(1);
        config.auto_select = true;
        let mut l = Launcher::new(&config, apps());
        assert!(l.type_text("t").finish.is_none());
        assert!(l.type_text("e").finish.is_some());
    }

    #[test]
    fn dmenu_codes_and_hide_before_typing() {
        let mut config = Config::defaults(1);
        config.dmenu.enabled = true;
        config.hide_before_typing = true;
        let mut l = Launcher::new(&config, apps());
        assert!(!l.list_shown());
        assert_eq!(l.action(Action::Cancel).finish, Some(Finish::Cancel(2)));
        let _ = l.type_text("c");
        assert!(l.list_shown());
        let _ = l.action(Action::DeletePrev);
        // Once shown, it stays shown, as fuzzel's flag is cleared for good.
        assert!(l.list_shown());
    }
}
