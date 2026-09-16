//! zsh's options (`options.c`): the option table, emulation, `setopt` and
//! `unsetopt`, and the option letters.
//!
//! The option numbers and the table are generated from zsh 5.9's `zsh.h`
//! and `options.c`, so they cannot drift from the reference.

use crate::shell::Shell;
use crate::utils::lossy;

pub(crate) const EMULATE_CSH: u32 = 1 << 1;
pub(crate) const EMULATE_KSH: u32 = 1 << 2;
pub(crate) const EMULATE_SH: u32 = 1 << 3;
pub(crate) const EMULATE_ZSH: u32 = 1 << 4;
pub(crate) const EMULATE_FULLY: u32 = 1 << 5;
const EMULATE_UNUSED: u32 = 1 << 6;

const F_CSH: u32 = EMULATE_CSH;
const F_KSH: u32 = EMULATE_KSH;
const F_SH: u32 = EMULATE_SH;
const F_ZSH: u32 = EMULATE_ZSH;
const F_ALL: u32 = F_CSH | F_KSH | F_SH | F_ZSH;
const F_BOURNE: u32 = F_KSH | F_SH;
const F_BSHELL: u32 = F_KSH | F_SH | F_ZSH;
const F_NONBOURNE: u32 = F_ALL & !F_BOURNE;
const F_NONZSH: u32 = F_ALL & !F_ZSH;
/// The option is relevant to emulation.
pub(crate) const F_EMULATE: u32 = EMULATE_UNUSED;
/// The option is never set by `emulate`.
pub(crate) const F_SPECIAL: u32 = EMULATE_UNUSED << 1;
/// The option is an alias for another.
pub(crate) const F_ALIAS: u32 = EMULATE_UNUSED << 2;

pub(crate) const ALIASESOPT: usize = 1;
pub(crate) const ALIASFUNCDEF: usize = 2;
pub(crate) const ALLEXPORT: usize = 3;
pub(crate) const ALWAYSLASTPROMPT: usize = 4;
pub(crate) const ALWAYSTOEND: usize = 5;
pub(crate) const APPENDHISTORY: usize = 6;
pub(crate) const AUTOCD: usize = 7;
pub(crate) const AUTOCONTINUE: usize = 8;
pub(crate) const AUTOLIST: usize = 9;
pub(crate) const AUTOMENU: usize = 10;
pub(crate) const AUTONAMEDIRS: usize = 11;
pub(crate) const AUTOPARAMKEYS: usize = 12;
pub(crate) const AUTOPARAMSLASH: usize = 13;
pub(crate) const AUTOPUSHD: usize = 14;
pub(crate) const AUTOREMOVESLASH: usize = 15;
pub(crate) const AUTORESUME: usize = 16;
pub(crate) const BADPATTERN: usize = 17;
pub(crate) const BANGHIST: usize = 18;
pub(crate) const BAREGLOBQUAL: usize = 19;
pub(crate) const BASHAUTOLIST: usize = 20;
pub(crate) const BASHREMATCH: usize = 21;
pub(crate) const BEEP: usize = 22;
pub(crate) const BGNICE: usize = 23;
pub(crate) const BRACECCL: usize = 24;
pub(crate) const BSDECHO: usize = 25;
pub(crate) const CASEGLOB: usize = 26;
pub(crate) const CASEMATCH: usize = 27;
pub(crate) const CASEPATHS: usize = 28;
pub(crate) const CBASES: usize = 29;
pub(crate) const CDABLEVARS: usize = 30;
pub(crate) const CDSILENT: usize = 31;
pub(crate) const CHASEDOTS: usize = 32;
pub(crate) const CHASELINKS: usize = 33;
pub(crate) const CHECKJOBS: usize = 34;
pub(crate) const CHECKRUNNINGJOBS: usize = 35;
pub(crate) const CLOBBER: usize = 36;
pub(crate) const CLOBBEREMPTY: usize = 37;
pub(crate) const APPENDCREATE: usize = 38;
pub(crate) const COMBININGCHARS: usize = 39;
pub(crate) const COMPLETEALIASES: usize = 40;
pub(crate) const COMPLETEINWORD: usize = 41;
pub(crate) const CORRECT: usize = 42;
pub(crate) const CORRECTALL: usize = 43;
pub(crate) const CONTINUEONERROR: usize = 44;
pub(crate) const CPRECEDENCES: usize = 45;
pub(crate) const CSHJUNKIEHISTORY: usize = 46;
pub(crate) const CSHJUNKIELOOPS: usize = 47;
pub(crate) const CSHJUNKIEQUOTES: usize = 48;
pub(crate) const CSHNULLCMD: usize = 49;
pub(crate) const CSHNULLGLOB: usize = 50;
pub(crate) const DEBUGBEFORECMD: usize = 51;
pub(crate) const EMACSMODE: usize = 52;
pub(crate) const EQUALS: usize = 53;
pub(crate) const ERREXIT: usize = 54;
pub(crate) const ERRRETURN: usize = 55;
pub(crate) const EXECOPT: usize = 56;
pub(crate) const EXTENDEDGLOB: usize = 57;
pub(crate) const EXTENDEDHISTORY: usize = 58;
pub(crate) const EVALLINENO: usize = 59;
pub(crate) const FLOWCONTROL: usize = 60;
pub(crate) const FORCEFLOAT: usize = 61;
pub(crate) const FUNCTIONARGZERO: usize = 62;
pub(crate) const GLOBOPT: usize = 63;
pub(crate) const GLOBALEXPORT: usize = 64;
pub(crate) const GLOBALRCS: usize = 65;
pub(crate) const GLOBASSIGN: usize = 66;
pub(crate) const GLOBCOMPLETE: usize = 67;
pub(crate) const GLOBDOTS: usize = 68;
pub(crate) const GLOBSTARSHORT: usize = 69;
pub(crate) const GLOBSUBST: usize = 70;
pub(crate) const HASHCMDS: usize = 71;
pub(crate) const HASHDIRS: usize = 72;
pub(crate) const HASHEXECUTABLESONLY: usize = 73;
pub(crate) const HASHLISTALL: usize = 74;
pub(crate) const HISTALLOWCLOBBER: usize = 75;
pub(crate) const HISTBEEP: usize = 76;
pub(crate) const HISTEXPIREDUPSFIRST: usize = 77;
pub(crate) const HISTFCNTLLOCK: usize = 78;
pub(crate) const HISTFINDNODUPS: usize = 79;
pub(crate) const HISTIGNOREALLDUPS: usize = 80;
pub(crate) const HISTIGNOREDUPS: usize = 81;
pub(crate) const HISTIGNORESPACE: usize = 82;
pub(crate) const HISTLEXWORDS: usize = 83;
pub(crate) const HISTNOFUNCTIONS: usize = 84;
pub(crate) const HISTNOSTORE: usize = 85;
pub(crate) const HISTREDUCEBLANKS: usize = 86;
pub(crate) const HISTSAVEBYCOPY: usize = 87;
pub(crate) const HISTSAVENODUPS: usize = 88;
pub(crate) const HISTSUBSTPATTERN: usize = 89;
pub(crate) const HISTVERIFY: usize = 90;
pub(crate) const HUP: usize = 91;
pub(crate) const IGNOREBRACES: usize = 92;
pub(crate) const IGNORECLOSEBRACES: usize = 93;
pub(crate) const IGNOREEOF: usize = 94;
pub(crate) const INCAPPENDHISTORY: usize = 95;
pub(crate) const INCAPPENDHISTORYTIME: usize = 96;
pub(crate) const INTERACTIVE: usize = 97;
pub(crate) const INTERACTIVECOMMENTS: usize = 98;
pub(crate) const KSHARRAYS: usize = 99;
pub(crate) const KSHAUTOLOAD: usize = 100;
pub(crate) const KSHGLOB: usize = 101;
pub(crate) const KSHOPTIONPRINT: usize = 102;
pub(crate) const KSHTYPESET: usize = 103;
pub(crate) const KSHZEROSUBSCRIPT: usize = 104;
pub(crate) const LISTAMBIGUOUS: usize = 105;
pub(crate) const LISTBEEP: usize = 106;
pub(crate) const LISTPACKED: usize = 107;
pub(crate) const LISTROWSFIRST: usize = 108;
pub(crate) const LISTTYPES: usize = 109;
pub(crate) const LOCALLOOPS: usize = 110;
pub(crate) const LOCALOPTIONS: usize = 111;
pub(crate) const LOCALPATTERNS: usize = 112;
pub(crate) const LOCALTRAPS: usize = 113;
pub(crate) const LOGINSHELL: usize = 114;
pub(crate) const LONGLISTJOBS: usize = 115;
pub(crate) const MAGICEQUALSUBST: usize = 116;
pub(crate) const MAILWARNING: usize = 117;
pub(crate) const MARKDIRS: usize = 118;
pub(crate) const MENUCOMPLETE: usize = 119;
pub(crate) const MONITOR: usize = 120;
pub(crate) const MULTIBYTE: usize = 121;
pub(crate) const MULTIFUNCDEF: usize = 122;
pub(crate) const MULTIOS: usize = 123;
pub(crate) const NOMATCH: usize = 124;
pub(crate) const NOTIFY: usize = 125;
pub(crate) const NULLGLOB: usize = 126;
pub(crate) const NUMERICGLOBSORT: usize = 127;
pub(crate) const OCTALZEROES: usize = 128;
pub(crate) const OVERSTRIKE: usize = 129;
pub(crate) const PATHDIRS: usize = 130;
pub(crate) const PATHSCRIPT: usize = 131;
pub(crate) const PIPEFAIL: usize = 132;
pub(crate) const POSIXALIASES: usize = 133;
pub(crate) const POSIXARGZERO: usize = 134;
pub(crate) const POSIXBUILTINS: usize = 135;
pub(crate) const POSIXCD: usize = 136;
pub(crate) const POSIXIDENTIFIERS: usize = 137;
pub(crate) const POSIXJOBS: usize = 138;
pub(crate) const POSIXSTRINGS: usize = 139;
pub(crate) const POSIXTRAPS: usize = 140;
pub(crate) const PRINTEIGHTBIT: usize = 141;
pub(crate) const PRINTEXITVALUE: usize = 142;
pub(crate) const PRIVILEGED: usize = 143;
pub(crate) const PROMPTBANG: usize = 144;
pub(crate) const PROMPTCR: usize = 145;
pub(crate) const PROMPTPERCENT: usize = 146;
pub(crate) const PROMPTSP: usize = 147;
pub(crate) const PROMPTSUBST: usize = 148;
pub(crate) const PUSHDIGNOREDUPS: usize = 149;
pub(crate) const PUSHDMINUS: usize = 150;
pub(crate) const PUSHDSILENT: usize = 151;
pub(crate) const PUSHDTOHOME: usize = 152;
pub(crate) const RCEXPANDPARAM: usize = 153;
pub(crate) const RCQUOTES: usize = 154;
pub(crate) const RCS: usize = 155;
pub(crate) const RECEXACT: usize = 156;
pub(crate) const REMATCHPCRE: usize = 157;
pub(crate) const RESTRICTED: usize = 158;
pub(crate) const RMSTARSILENT: usize = 159;
pub(crate) const RMSTARWAIT: usize = 160;
pub(crate) const SHAREHISTORY: usize = 161;
pub(crate) const SHFILEEXPANSION: usize = 162;
pub(crate) const SHGLOB: usize = 163;
pub(crate) const SHINSTDIN: usize = 164;
pub(crate) const SHNULLCMD: usize = 165;
pub(crate) const SHOPTIONLETTERS: usize = 166;
pub(crate) const SHORTLOOPS: usize = 167;
pub(crate) const SHORTREPEAT: usize = 168;
pub(crate) const SHWORDSPLIT: usize = 169;
pub(crate) const SINGLECOMMAND: usize = 170;
pub(crate) const SINGLELINEZLE: usize = 171;
pub(crate) const SOURCETRACE: usize = 172;
pub(crate) const SUNKEYBOARDHACK: usize = 173;
pub(crate) const TRANSIENTRPROMPT: usize = 174;
pub(crate) const TRAPSASYNC: usize = 175;
pub(crate) const TYPESETSILENT: usize = 176;
pub(crate) const TYPESETTOUNSET: usize = 177;
pub(crate) const UNSET: usize = 178;
pub(crate) const VERBOSE: usize = 179;
pub(crate) const VIMODE: usize = 180;
pub(crate) const WARNCREATEGLOBAL: usize = 181;
pub(crate) const WARNNESTEDVAR: usize = 182;
pub(crate) const XTRACE: usize = 183;
pub(crate) const USEZLE: usize = 184;
pub(crate) const DVORAK: usize = 185;
pub(crate) const OPT_SIZE: usize = 186;

/// `(name, flags, option number)`, as `optns[]` lists them; a negative number is a negated alias.
pub(crate) const OPTNS: &[(&str, u32, i32)] = &[
    ("aliases", F_EMULATE | F_ALL, 1),
    ("aliasfuncdef", F_EMULATE | F_BOURNE, 2),
    ("allexport", F_EMULATE, 3),
    ("alwayslastprompt", F_ALL, 4),
    ("alwaystoend", 0, 5),
    ("appendcreate", F_EMULATE | F_BOURNE, 38),
    ("appendhistory", F_ALL, 6),
    ("autocd", F_EMULATE, 7),
    ("autocontinue", 0, 8),
    ("autolist", F_ALL, 9),
    ("automenu", F_ALL, 10),
    ("autonamedirs", 0, 11),
    ("autoparamkeys", F_ALL, 12),
    ("autoparamslash", F_ALL, 13),
    ("autopushd", 0, 14),
    ("autoremoveslash", F_ALL, 15),
    ("autoresume", 0, 16),
    ("badpattern", F_EMULATE | F_NONBOURNE, 17),
    ("banghist", F_NONBOURNE, 18),
    ("bareglobqual", F_EMULATE | F_ZSH, 19),
    ("bashautolist", 0, 20),
    ("bashrematch", 0, 21),
    ("beep", F_ALL, 22),
    ("bgnice", F_EMULATE | F_NONBOURNE, 23),
    ("braceccl", F_EMULATE, 24),
    ("bsdecho", F_EMULATE | F_SH, 25),
    ("caseglob", F_ALL, 26),
    ("casematch", F_ALL, 27),
    ("casepaths", 0, 28),
    ("cbases", 0, 29),
    ("cprecedences", F_EMULATE | F_NONZSH, 45),
    ("cdablevars", F_EMULATE, 30),
    ("cdsilent", 0, 31),
    ("chasedots", F_EMULATE, 32),
    ("chaselinks", F_EMULATE, 33),
    ("checkjobs", F_EMULATE | F_ZSH, 34),
    ("checkrunningjobs", F_EMULATE | F_ZSH, 35),
    ("clobber", F_EMULATE | F_ALL, 36),
    ("clobberempty", 0, 37),
    ("combiningchars", 0, 39),
    ("completealiases", 0, 40),
    ("completeinword", 0, 41),
    ("continueonerror", 0, 44),
    ("correct", 0, 42),
    ("correctall", 0, 43),
    ("cshjunkiehistory", F_EMULATE | F_CSH, 46),
    ("cshjunkieloops", F_EMULATE | F_CSH, 47),
    ("cshjunkiequotes", F_EMULATE | F_CSH, 48),
    ("cshnullcmd", F_EMULATE | F_CSH, 49),
    ("cshnullglob", F_EMULATE | F_CSH, 50),
    ("debugbeforecmd", F_ALL, 51),
    ("emacs", 0, 52),
    ("equals", F_EMULATE | F_ZSH, 53),
    ("errexit", F_EMULATE, 54),
    ("errreturn", F_EMULATE, 55),
    ("exec", F_ALL, 56),
    ("extendedglob", F_EMULATE, 57),
    ("extendedhistory", F_CSH, 58),
    ("evallineno", F_EMULATE | F_ZSH, 59),
    ("flowcontrol", F_ALL, 60),
    ("forcefloat", 0, 61),
    ("functionargzero", F_EMULATE | F_NONBOURNE, 62),
    ("glob", F_EMULATE | F_ALL, 63),
    ("globalexport", F_EMULATE | F_ZSH, 64),
    ("globalrcs", F_ALL, 65),
    ("globassign", F_EMULATE | F_CSH, 66),
    ("globcomplete", 0, 67),
    ("globdots", F_EMULATE, 68),
    ("globstarshort", F_EMULATE, 69),
    ("globsubst", F_EMULATE | F_NONZSH, 70),
    ("hashcmds", F_ALL, 71),
    ("hashdirs", F_ALL, 72),
    ("hashexecutablesonly", 0, 73),
    ("hashlistall", F_ALL, 74),
    ("histallowclobber", 0, 75),
    ("histbeep", F_ALL, 76),
    ("histexpiredupsfirst", 0, 77),
    ("histfcntllock", 0, 78),
    ("histfindnodups", 0, 79),
    ("histignorealldups", 0, 80),
    ("histignoredups", 0, 81),
    ("histignorespace", 0, 82),
    ("histlexwords", 0, 83),
    ("histnofunctions", 0, 84),
    ("histnostore", 0, 85),
    ("histsubstpattern", F_EMULATE, 89),
    ("histreduceblanks", 0, 86),
    ("histsavebycopy", F_ALL, 87),
    ("histsavenodups", 0, 88),
    ("histverify", 0, 90),
    ("hup", F_EMULATE | F_ZSH, 91),
    ("ignorebraces", F_EMULATE | F_SH, 92),
    ("ignoreclosebraces", F_EMULATE, 93),
    ("ignoreeof", 0, 94),
    ("incappendhistory", 0, 95),
    ("incappendhistorytime", 0, 96),
    ("interactive", F_SPECIAL, 97),
    ("interactivecomments", F_BOURNE, 98),
    ("ksharrays", F_EMULATE | F_BOURNE, 99),
    ("kshautoload", F_EMULATE | F_BOURNE, 100),
    ("kshglob", F_EMULATE | F_KSH, 101),
    ("kshoptionprint", F_EMULATE | F_KSH, 102),
    ("kshtypeset", 0, 103),
    ("kshzerosubscript", 0, 104),
    ("listambiguous", F_ALL, 105),
    ("listbeep", F_ALL, 106),
    ("listpacked", 0, 107),
    ("listrowsfirst", 0, 108),
    ("listtypes", F_ALL, 109),
    ("localoptions", F_EMULATE | F_KSH, 111),
    ("localloops", F_EMULATE, 110),
    ("localpatterns", F_EMULATE, 112),
    ("localtraps", F_EMULATE | F_KSH, 113),
    ("login", F_SPECIAL, 114),
    ("longlistjobs", 0, 115),
    ("magicequalsubst", F_EMULATE, 116),
    ("mailwarning", 0, 117),
    ("markdirs", 0, 118),
    ("menucomplete", 0, 119),
    ("monitor", F_SPECIAL, 120),
    ("multibyte", F_ALL, 121),
    ("multifuncdef", F_EMULATE | F_ZSH, 122),
    ("multios", F_EMULATE | F_ZSH, 123),
    ("nomatch", F_EMULATE | F_NONBOURNE, 124),
    ("notify", F_ZSH, 125),
    ("nullglob", F_EMULATE, 126),
    ("numericglobsort", F_EMULATE, 127),
    ("octalzeroes", F_EMULATE | F_SH, 128),
    ("overstrike", 0, 129),
    ("pathdirs", F_EMULATE, 130),
    ("pathscript", F_EMULATE | F_BOURNE, 131),
    ("pipefail", F_EMULATE, 132),
    ("posixaliases", F_EMULATE | F_BOURNE, 133),
    ("posixargzero", F_EMULATE, 134),
    ("posixbuiltins", F_EMULATE | F_BOURNE, 135),
    ("posixcd", F_EMULATE | F_BOURNE, 136),
    ("posixidentifiers", F_EMULATE | F_BOURNE, 137),
    ("posixjobs", F_EMULATE | F_BOURNE, 138),
    ("posixstrings", F_EMULATE | F_BOURNE, 139),
    ("posixtraps", F_EMULATE | F_BOURNE, 140),
    ("printeightbit", 0, 141),
    ("printexitvalue", 0, 142),
    ("privileged", F_SPECIAL, 143),
    ("promptbang", F_KSH, 144),
    ("promptcr", F_ALL, 145),
    ("promptpercent", F_NONBOURNE, 146),
    ("promptsp", F_ALL, 147),
    ("promptsubst", F_BOURNE, 148),
    ("pushdignoredups", F_EMULATE, 149),
    ("pushdminus", F_EMULATE, 150),
    ("pushdsilent", 0, 151),
    ("pushdtohome", F_EMULATE, 152),
    ("rcexpandparam", F_EMULATE, 153),
    ("rcquotes", F_EMULATE, 154),
    ("rcs", F_ALL, 155),
    ("recexact", 0, 156),
    ("rematchpcre", 0, 157),
    ("restricted", F_SPECIAL, 158),
    ("rmstarsilent", F_BOURNE, 159),
    ("rmstarwait", 0, 160),
    ("sharehistory", F_KSH, 161),
    ("shfileexpansion", F_EMULATE | F_BOURNE, 162),
    ("shglob", F_EMULATE | F_BOURNE, 163),
    ("shinstdin", F_SPECIAL, 164),
    ("shnullcmd", F_EMULATE | F_BOURNE, 165),
    ("shoptionletters", F_EMULATE | F_BOURNE, 166),
    ("shortloops", F_EMULATE | F_NONBOURNE, 167),
    ("shortrepeat", F_EMULATE, 168),
    ("shwordsplit", F_EMULATE | F_BOURNE, 169),
    ("singlecommand", F_SPECIAL, 170),
    ("singlelinezle", F_KSH, 171),
    ("sourcetrace", 0, 172),
    ("sunkeyboardhack", 0, 173),
    ("transientrprompt", 0, 174),
    ("trapsasync", 0, 175),
    ("typesetsilent", F_EMULATE | F_BOURNE, 176),
    ("typesettounset", F_EMULATE | F_BOURNE, 177),
    ("unset", F_EMULATE | F_BSHELL, 178),
    ("verbose", 0, 179),
    ("vi", 0, 180),
    ("warncreateglobal", F_EMULATE, 181),
    ("warnnestedvar", F_EMULATE, 182),
    ("xtrace", 0, 183),
    ("zle", F_SPECIAL, 184),
    ("braceexpand", F_ALIAS, -92),
    ("dotglob", F_ALIAS, 68),
    ("hashall", F_ALIAS, 71),
    ("histappend", F_ALIAS, 6),
    ("histexpand", F_ALIAS, 18),
    ("log", F_ALIAS, -84),
    ("mailwarn", F_ALIAS, 117),
    ("onecmd", F_ALIAS, 170),
    ("physical", F_ALIAS, 33),
    ("promptvars", F_ALIAS, 148),
    ("stdin", F_ALIAS, 164),
    ("trackall", F_ALIAS, 71),
    ("dvorak", 0, 185),
];

pub(crate) const ZSHLETTERS: [i32; 74] = [
    42, 142, -17, -124, 68, 125, 23, 94, 118, 9, 0, 0, 0, 0, 0, 0, 0, 0, -22, -36, 152, 151, -63,
    126, 159, 92, 7, -18, 173, 171, 14, 43, 153, 130, 115, 156, 30, 117, -145, 16, 109, 119, 184,
    0, 0, 0, 0, 0, 0, 3, 0, 0, -65, 54, -155, 82, 81, 97, 0, 98, 114, 120, -56, 0, 143, 0, 158,
    164, 170, -178, 179, 33, 183, 169,
];
pub(crate) const KSHLETTERS: [i32; 74] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, -36, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 175, 0, 0, 0, 118, 0, 0, 0, 0, 0, 0, 0, 0, 3, 125, 0, 0, 54, -63, 0, 0, 97, 0,
    0, 114, 120, -56, 0, 143, 0, 158, 164, 170, -178, 179, 0, 183, 0,
];

const FIRST_OPT: u8 = b'0';
const LAST_OPT: u8 = b'y';

/// One row of the option table.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OptName {
    pub(crate) name: &'static str,
    pub(crate) flags: u32,
    pub(crate) optno: i32,
}

impl Shell {
    /// `isset(X)`.
    #[inline]
    pub(crate) fn isset(&self, o: usize) -> bool {
        self.opts.get(o).is_some_and(|&v| v)
    }

    /// `unset(X)`.
    #[inline]
    pub(crate) fn unset_opt(&self, o: usize) -> bool {
        !self.isset(o)
    }

    /// `EMULATION(X)`.
    pub(crate) fn emulation_is(&self, x: u32) -> bool {
        self.emulation & x != 0
    }
}

/// zsh's `createoptiontable`.
pub(crate) fn createoptiontable() -> crate::hashtable::HashTable<OptName> {
    let mut t = crate::hashtable::HashTable::new(101);
    for &(name, flags, optno) in OPTNS {
        let _ = t.insert(name.as_bytes().to_vec(), OptName { name, flags, optno });
    }
    t
}

/// `defset(X, emulation)`.
fn defset(on: &OptName, emulation: u32) -> bool {
    on.flags & emulation != 0
}

/// zsh's `installemulation`: set `opts` to what `emulation` gives.
pub(crate) fn installemulation(emulation: u32, opts: &mut [bool; OPT_SIZE]) {
    let fully = emulation & EMULATE_FULLY != 0;
    for &(_, flags, optno) in OPTNS {
        if flags & F_ALIAS == 0
            && ((fully && flags & F_SPECIAL == 0) || flags & F_EMULATE != 0)
            && let Some(slot) = usize::try_from(optno).ok().and_then(|o| opts.get_mut(o))
        {
            *slot = flags & emulation != 0;
        }
    }
}

/// zsh's `emulate`: the emulation `name` asks for, installed into `opts`.
pub(crate) fn emulate(sh: &Shell, name: &[u8], fully: bool, opts: &mut [bool; OPT_SIZE]) -> u32 {
    let mut ch = name.first().copied().unwrap_or(0);
    if ch == b'r' {
        ch = name.get(1).copied().unwrap_or(0);
    }
    let mut e = match ch {
        b'c' => EMULATE_CSH,
        b'k' => EMULATE_KSH,
        b's' | b'b' => EMULATE_SH,
        _ => EMULATE_ZSH,
    };
    if fully {
        e |= EMULATE_FULLY;
    }
    installemulation(e, opts);
    if sh.current_function_traced() {
        opts[XTRACE] = true;
    }
    e
}

/// zsh's `optlookup`: the option number (negative for a `no` form), 0 if
/// there is no such option.
pub(crate) fn optlookup(sh: &Shell, name: &[u8]) -> i32 {
    let s: Vec<u8> = name
        .iter()
        .filter(|&&c| c != b'_')
        .map(u8::to_ascii_lowercase)
        .collect();
    if s.starts_with(b"no")
        && let Some(n) = sh.optiontab.get(s.get(2..).unwrap_or(&[]))
    {
        return -n.optno;
    }
    sh.optiontab.get(&s).map_or(0, |n| n.optno)
}

/// zsh's `optlookupc`.
pub(crate) fn optlookupc(sh: &Shell, c: u8) -> i32 {
    if !(FIRST_OPT..=LAST_OPT).contains(&c) {
        return 0;
    }
    let table = if sh.isset(SHOPTIONLETTERS) {
        &KSHLETTERS
    } else {
        &ZSHLETTERS
    };
    table.get(usize::from(c - FIRST_OPT)).copied().unwrap_or(0)
}

impl Shell {
    /// zsh's `dosetopt` on the shell's own options. Returns 0, or -1 when
    /// the option cannot be changed.
    pub(crate) fn dosetopt(&mut self, optno: i32, value: bool, force: bool) -> i32 {
        let mut opts = self.opts;
        let r = self.dosetopt_in(optno, value, force, &mut opts);
        self.opts = opts;
        if r == 0 {
            let o = usize::try_from(optno.unsigned_abs()).unwrap_or(0);
            if o == BANGHIST || o == SHINSTDIN {
                self.inittyptab();
            }
        }
        r
    }

    /// zsh's `dosetopt` into an option array.
    pub(crate) fn dosetopt_in(
        &mut self,
        optno: i32,
        value: bool,
        force: bool,
        new_opts: &mut [bool; OPT_SIZE],
    ) -> i32 {
        if optno == 0 {
            return -1;
        }
        let value = if optno < 0 { !value } else { value };
        let o = usize::try_from(optno.unsigned_abs()).unwrap_or(0);
        let cur = new_opts.get(o).copied().unwrap_or(false);
        if o == RESTRICTED {
            if self.isset(RESTRICTED) {
                return if value { 0 } else { -1 };
            }
        } else if !force && o == EXECOPT && !value && self.isset(INTERACTIVE) {
            return -1;
        } else if !force && (o == INTERACTIVE || o == SHINSTDIN || o == SINGLECOMMAND) {
            return if cur == value { 0 } else { -1 };
        } else if !force && o == USEZLE && value {
            if !self.isset(INTERACTIVE) || self.shtty < 0 {
                return -1;
            }
        } else if o == PRIVILEGED && !value {
            // SAFETY: getgid/getuid have no preconditions.
            let gid = unsafe { libc::getgid() };
            // SAFETY: as above.
            let uid = unsafe { libc::getuid() };
            // SAFETY: setresgid has no memory-safety preconditions.
            if unsafe { libc::setresgid(gid, gid, gid) } != 0 {
                self.zwarnnam(
                    "unsetopt",
                    "PRIVILEGED: can't drop privileges; failed to change group ID",
                );
                return -1;
            }
            // SAFETY: setresuid has no memory-safety preconditions.
            if unsafe { libc::setresuid(uid, uid, uid) } != 0 {
                self.zwarnnam(
                    "unsetopt",
                    "PRIVILEGED: can't drop privileges; failed to change user ID",
                );
                return -1;
            }
        } else if !force && o == MONITOR && value {
            if cur == value {
                return 0;
            }
            if self.shtty >= 0 {
                self.acquire_pgrp();
            } else {
                return -1;
            }
        } else if (o == EMACSMODE || o == VIMODE) && value {
            if self.sticky.as_ref().is_some_and(|s| s.emulation != 0) {
                return -1;
            }
            self.zle_set_keymap_for_option(o);
            if let Some(slot) = new_opts.get_mut(if o == EMACSMODE { VIMODE } else { EMACSMODE }) {
                *slot = false;
            }
        } else if o == SUNKEYBOARDHACK {
            self.keyboardhackchar = if value { b'`' } else { 0 };
        }
        if let Some(slot) = new_opts.get_mut(o) {
            *slot = value;
        }
        0
    }

    /// `$-`: the letters of the options that are on.
    pub(crate) fn dashgetfn(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for c in FIRST_OPT..=LAST_OPT {
            let optno = optlookupc(self, c);
            if optno != 0 {
                let o = usize::try_from(optno.unsigned_abs()).unwrap_or(0);
                if (optno > 0) == self.isset(o) {
                    out.push(c);
                }
            }
        }
        out
    }

    /// `setopt`/`unsetopt` with no arguments.
    pub(crate) fn print_options(&self, set: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for name in self.optiontab.sorted_keys() {
            let Some(on) = self.optiontab.get(&name) else {
                continue;
            };
            if on.flags & F_ALIAS != 0 {
                continue;
            }
            let optno = usize::try_from(on.optno.unsigned_abs()).unwrap_or(0);
            let isset = self.isset(optno);
            if self.isset(KSHOPTIONPRINT) {
                if defset(on, self.emulation) {
                    out.extend(
                        format!("no{:<19} {}\n", on.name, if isset { "off" } else { "on" }).bytes(),
                    );
                } else {
                    out.extend(
                        format!("{:<21} {}\n", on.name, if isset { "on" } else { "off" }).bytes(),
                    );
                }
            } else if set == (isset ^ defset(on, self.emulation)) {
                if set ^ isset {
                    out.extend_from_slice(b"no");
                }
                out.extend_from_slice(on.name.as_bytes());
                out.push(b'\n');
            }
        }
        out
    }

    /// `set -o` (`hadplus` false) and `set +o`.
    pub(crate) fn print_option_states(&self, hadplus: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for name in self.optiontab.sorted_keys() {
            let Some(on) = self.optiontab.get(&name) else {
                continue;
            };
            if on.flags & F_ALIAS != 0 {
                continue;
            }
            let optno = usize::try_from(on.optno.unsigned_abs()).unwrap_or(0);
            let isset = self.isset(optno);
            let def = defset(on, self.emulation);
            if hadplus {
                out.extend(
                    format!(
                        "set {}o {}{}\n",
                        if def != isset { '-' } else { '+' },
                        if def { "no" } else { "" },
                        on.name
                    )
                    .bytes(),
                );
            } else if def {
                out.extend(
                    format!("no{:<19} {}\n", on.name, if isset { "off" } else { "on" }).bytes(),
                );
            } else {
                out.extend(
                    format!("{:<21} {}\n", on.name, if isset { "on" } else { "off" }).bytes(),
                );
            }
        }
        out
    }

    /// `emulate -l`.
    pub(crate) fn list_emulate_options(&self, cmdopts: &[bool; OPT_SIZE], fully: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for name in self.optiontab.sorted_keys() {
            let Some(on) = self.optiontab.get(&name) else {
                continue;
            };
            if on.flags & F_ALIAS == 0
                && ((fully && on.flags & F_SPECIAL == 0) || on.flags & F_EMULATE != 0)
            {
                let o = usize::try_from(on.optno.unsigned_abs()).unwrap_or(0);
                if !cmdopts.get(o).copied().unwrap_or(false) {
                    out.extend_from_slice(b"no");
                }
                out.extend_from_slice(on.name.as_bytes());
                out.push(b'\n');
            }
        }
        out
    }

    /// zsh's `bin_setopt`.
    pub(crate) fn bin_setopt(&mut self, nam: &str, args: &[Vec<u8>], isun: bool) -> i32 {
        if args.is_empty() {
            let text = self.print_options(!isun);
            self.write_stdout(&text);
            return 0;
        }
        let mut retval = 0;
        let mut k = 0;
        let mut do_match = false;
        'opts: while let Some(arg) = args.get(k) {
            let first = arg.first().copied().unwrap_or(0);
            if first != b'-' && first != b'+' {
                break;
            }
            let action = (first == b'-') ^ isun;
            let letters: Vec<u8> = if arg.len() == 1 {
                b"-".to_vec()
            } else {
                crate::tok::unmetafy(arg.get(1..).unwrap_or(&[]))
            };
            let mut j = 0;
            while let Some(&c) = letters.get(j) {
                if c == b'-' {
                    k += 1;
                    break 'opts;
                } else if c == b'o' {
                    let name: Vec<u8> = if j + 1 < letters.len() {
                        crate::tok::metafy(letters.get(j + 1..).unwrap_or(&[]))
                    } else {
                        k += 1;
                        if let Some(a) = args.get(k) {
                            a.clone()
                        } else {
                            self.zwarnnam(nam, "string expected after -o");
                            self.inittyptab();
                            return 1;
                        }
                    };
                    let optno = optlookup(self, &name);
                    if optno == 0 {
                        self.zwarnnam(nam, &format!("no such option: {}", lossy(&name)));
                        retval |= 1;
                    } else if self.dosetopt(optno, action, false) != 0 {
                        self.zwarnnam(nam, &format!("can't change option: {}", lossy(&name)));
                        retval |= 1;
                    }
                    break;
                } else if c == b'm' {
                    do_match = true;
                } else {
                    let optno = optlookupc(self, c);
                    if optno == 0 {
                        self.zwarnnam(nam, &format!("bad option: -{}", char::from(c)));
                        retval |= 1;
                    } else if self.dosetopt(optno, action, false) != 0 {
                        self.zwarnnam(nam, &format!("can't change option: -{}", char::from(c)));
                        retval |= 1;
                    }
                }
                j += 1;
            }
            k += 1;
        }
        let rest = args.get(k..).unwrap_or(&[]);
        if do_match {
            for a in rest {
                let mut s: Vec<u8> = a
                    .iter()
                    .filter(|&&c| c != b'_')
                    .map(u8::to_ascii_lowercase)
                    .collect();
                crate::pattern::tokenize(&mut s);
                let Some(prog) = self.patcompile(&s, 0, None) else {
                    self.zwarnnam(nam, &format!("bad pattern: {}", lossy(a)));
                    retval |= 1;
                    break;
                };
                for name in self.optiontab.keys() {
                    let Some(on) = self.optiontab.get(&name).copied() else {
                        continue;
                    };
                    if on.flags & F_ALIAS == 0 && self.pattry(&prog, &name) {
                        let _ = self.dosetopt(on.optno, !isun, false);
                    }
                }
            }
        } else {
            for a in rest {
                let optno = optlookup(self, a);
                if optno == 0 {
                    self.zwarnnam(nam, &format!("no such option: {}", lossy(a)));
                    retval |= 1;
                } else if self.dosetopt(optno, !isun, false) != 0 {
                    self.zwarnnam(nam, &format!("can't change option: {}", lossy(a)));
                    retval |= 1;
                }
            }
        }
        self.inittyptab();
        retval
    }
}
