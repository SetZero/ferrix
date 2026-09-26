"""Read the SysML v2 subset `docs/sysml/` is written in.

There is no SysML v2 parser in this tree's toolchain and no Debian package for
one; the implementations that exist are a paid licence, a Java pilot, or a pip
install into a virtualenv. None of those can be a gate here, because the gates
in this repository install nothing and run on a stock checkout.

What makes a parser affordable anyway is that the model is written in a narrow,
consistent slice of the notation, by hand, in one style. So this is not a
grammar for the language -- it is a reader for *this* model:

  1. Comments are lifted out, remembering which were `doc` comments.
  2. The text is walked for `{`, `}` and `;` outside strings, which gives the
     element tree directly, because the textual notation is brace structured.
  3. Each declaration header is tokenised into keywords, modifiers, kind, name,
     type and value.

The third step is the only one that can fall behind the model, and it fails
*loudly*: a header it does not recognise becomes an element with an empty
`kind` and its raw text intact, and `gen-arch-doc.py` refuses to write a
document while any exist unless `--lenient` is passed. A parser that guessed
would drop a part from the structure section and nobody would see the hole.

One rule is worth stating because it is the whole of step 1: in this notation a
`doc` comment sits *inside* the body of the thing it describes, never in front
of it. So a doc comment opening a body belongs to the element that body
belongs to -- not to the declaration that happens to follow it.

The subset covered, which is everything the thirteen files use:

    package / part / action / item / attribute / port / interface / state /
    enum / requirement / verification / metadata / view / viewpoint /
    constraint / subject / objective, in both `def` and usage form;
    `abstract`, `variation`, `variant`, `ref`, `perform`, `exhibit`, `assert`;
    short names in angle brackets; `:`, `:>`, `:>>`; multiplicities; `=`
    values including sequences and constructor calls; enum literals;
    `first`/`then` action flows and the `decide` / `if` / `else` branches
    between them; `transition`; `dependency`, `satisfy`, `allocate`,
    `verify`, `connect`, `import`, `expose`, `filter`; and the `@stage` /
    `@deferred` annotation bodies.
"""

from __future__ import annotations

import hashlib
import pathlib
import re

from .model import Element, Model, Relation

# Element kinds the notation puts before a name.
KINDS = {
    "package",
    "part",
    "action",
    "item",
    "attribute",
    "port",
    "interface",
    "state",
    "enum",
    "requirement",
    "verification",
    "metadata",
    "view",
    "viewpoint",
    "constraint",
    "calc",
    "concern",
    "connection",
    "occurrence",
    "subject",
    "objective",
}

# Words that may sit in front of the kind.
MODIFIERS = {
    "abstract",
    "variation",
    "variant",
    "ref",
    "readonly",
    "derived",
    "end",
    "individual",
    "snapshot",
    "timeslice",
    "perform",
    "exhibit",
    "assert",
    "private",
    "public",
    "protected",
    "in",
    "out",
    "inout",
    "return",
    "do",
    "then",
    "first",
}

# Statements that draw an edge rather than declare an element.
RELATION_HEADS = {
    "dependency",
    "satisfy",
    "allocate",
    "verify",
    "connect",
    "import",
    "expose",
    "filter",
    "include",
}

# Statement heads whose body is control flow, not a declaration. They are kept
# as opaque clauses so the element tree stays clean: the document renders an
# action's *order*, which `flow` already carries, not its guards.
CLAUSE_HEADS = {"if", "else", "decide", "accept", "entry", "send", "assign"}

# `transition t first a accept trigger then b` -- the one clause that is not
# control flow but an edge, and the only statement in the notation that names
# both of its ends. A state machine is undrawable without it, so it parses into
# an element whose `flow` is [source, target] rather than into opaque text.
_TRANSITION = re.compile(
    r"^transition\b\s*(?:<\s*'[^']*'\s*>\s*)?(\w+)?\s*"
    r"first\s+([\w.]+)\s*"
    r"(?:accept\s+(.*?)\s*)?"
    r"then\s+([\w.]+)\s*$",
    re.S,
)

# Trailing feature words that are not part of a type name.
TYPE_TAIL = {"ordered", "nonunique", "unique", "ordered;"}

# The forms a succession takes in an action body. `first a then b` states both
# ends; `then b` continues from whatever came before it; `if g then b` and
# `else b` are the two ways out of a `decide`.
_FIRST_THEN = re.compile(r"^first\s+([\w.]+)\s+then\s+(?:action\s+)?([\w.]+)")
_FIRST = re.compile(r"^first\s+(?:action\s+)?([\w.]+)\s*$")
_THEN = re.compile(r"^then\s+(?:action\s+)?([\w.]+)")
_IF_THEN = re.compile(r"^if\s+(.*?)\s+then\s+([\w.]+)\s*$", re.S)
_ELSE = re.compile(r"^else\s+([\w.]+)\s*$")

_STAGE = re.compile(r"@stage\s*\{\s*number\s*=\s*(\d+)\s*;?\s*\}")
_DEFERRED = re.compile(r'@deferred\s*\{\s*reason\s*=\s*"(.*?)"\s*;?\s*\}', re.S)
_SHORT_NAME = re.compile(r"<\s*'([^']*)'\s*>")
_MULTIPLICITY = re.compile(r"\[([^\]]*)\]")
_PLACEHOLDER = re.compile(r"\x00(\d+)\x00")
_IDENTIFIER = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


class _Scanner:
    """Split a file into code, with comments pulled aside.

    Block comments are replaced by a placeholder so the brace walk never sees a
    `{` inside prose -- and the model's prose is full of them. Each comment is
    remembered along with whether the word `doc` introduced it, because that is
    what separates documentation from a section banner.
    """

    def __init__(self, text: str) -> None:
        self.text = text
        self.comments: list[tuple[bool, str]] = []

    def run(self) -> str:
        out: list[str] = []
        index = 0
        length = len(self.text)
        while index < length:
            char = self.text[index]
            if char == '"':
                end = self._end_of_string(index)
                out.append(self.text[index:end])
                index = end
            elif self.text.startswith("//", index):
                end = self.text.find("\n", index)
                index = length if end == -1 else end
            elif self.text.startswith("/*", index):
                end = self.text.find("*/", index + 2)
                end = length if end == -1 else end + 2
                body = self.text[index:end]
                is_doc = self._preceded_by_doc(index)
                self.comments.append((is_doc, _reflow(body)))
                out.append(f"\x00{len(self.comments) - 1}\x00")
                # Preserve the line count the comment spanned.
                out.append("\n" * body.count("\n"))
                index = end
            else:
                out.append(char)
                index += 1
        return "".join(out)

    def _preceded_by_doc(self, start: int) -> bool:
        before = self.text[:start].rstrip()
        return bool(re.search(r"(?:^|[^A-Za-z0-9_])doc$", before))

    def _end_of_string(self, start: int) -> int:
        index = start + 1
        while index < len(self.text):
            if self.text[index] == "\\":
                index += 2
                continue
            if self.text[index] == '"':
                return index + 1
            index += 1
        return len(self.text)


def _reflow(body: str) -> str:
    """Turn a `/* ... */` block into paragraphs of plain prose.

    The model writes doc comments as a left-aligned column of ` * ` prefixes
    hard-wrapped at about seventy columns. Those wraps belong to the source
    file, not to the sentence, so they are undone; a blank line stays a
    paragraph break.
    """
    inner = body
    if inner.startswith("/*"):
        inner = inner[2:]
    if inner.endswith("*/"):
        inner = inner[:-2]
    lines: list[str] = []
    for line in inner.splitlines():
        stripped = line.strip()
        if stripped.startswith("*"):
            stripped = stripped[1:].strip()
        lines.append(stripped)
    paragraphs: list[list[str]] = [[]]
    for line in lines:
        if not line:
            if paragraphs[-1]:
                paragraphs.append([])
            continue
        paragraphs[-1].append(line)
    return "\n\n".join(" ".join(words) for words in paragraphs if words)


def split_top_level(text: str, separator: str = ",") -> list[str]:
    """Split on a separator that is not inside brackets, braces or a string."""
    parts: list[str] = []
    depth = 0
    in_string = False
    current: list[str] = []
    index = 0
    while index < len(text):
        char = text[index]
        if in_string:
            current.append(char)
            if char == "\\" and index + 1 < len(text):
                current.append(text[index + 1])
                index += 2
                continue
            if char == '"':
                in_string = False
            index += 1
            continue
        if char == '"':
            in_string = True
            current.append(char)
        elif char in "([{":
            depth += 1
            current.append(char)
        elif char in ")]}":
            depth -= 1
            current.append(char)
        elif char == separator and depth == 0:
            parts.append("".join(current).strip())
            current = []
        else:
            current.append(char)
        index += 1
    tail = "".join(current).strip()
    if tail:
        parts.append(tail)
    return [part for part in parts if part]


class _Parser:
    def __init__(self, source: str, text: str) -> None:
        scanner = _Scanner(text)
        self.code = scanner.run()
        self.comments = scanner.comments
        self.source = source
        self.root = Element(kind="file", name="", source=source)
        self.relations: list[Relation] = []
        self.unparsed: list[Element] = []
        self.previous: dict[int, str] = {}
        """Per action body, the step a bare `then` continues from."""
        self.branch: dict[int, str] = {}
        """Per action body, the `decide` an `if`/`else` belongs to."""

    # -- the brace walk ---------------------------------------------------

    def run(self) -> Element:
        stack = [self.root]
        buffer: list[str] = []
        line = 1
        index = 0
        while index < len(self.code):
            char = self.code[index]
            if char == "\n":
                line += 1
                buffer.append(char)
                index += 1
                continue
            if char == '"':
                end = index + 1
                while end < len(self.code) and self.code[end] != '"':
                    end += 2 if self.code[end] == "\\" else 1
                buffer.append(self.code[index : end + 1])
                index = end + 1
                continue
            if char == "{":
                header = "".join(buffer).strip()
                buffer = []
                stack.append(self._declare(header, stack[-1], line))
            elif char == "}":
                closed = stack.pop() if len(stack) > 1 else stack[0]
                # A body whose last thing is its `doc` comment leaves it in the
                # buffer with no `;` to flush it, so claim it before it is lost.
                trailing = "".join(buffer).strip()
                if trailing:
                    self._statement(trailing, closed, line)
                self._fold_annotations(closed)
                buffer = []
            elif char == ";":
                statement = "".join(buffer).strip()
                buffer = []
                if statement:
                    self._statement(statement, stack[-1], line)
            else:
                buffer.append(char)
            index += 1
        return self.root

    # -- comments ---------------------------------------------------------

    def _take_comments(self, text: str, owner: Element) -> str:
        """Strip comment placeholders, giving `owner` the doc comment.

        A doc comment opening a body describes the body's owner, so the caller
        passes the *enclosing* element. A comment with content in front of it
        is a stray and is simply dropped.
        """
        while True:
            match = _PLACEHOLDER.search(text)
            if match is None:
                return text
            is_doc, body = self.comments[int(match.group(1))]
            before = text[: match.start()]
            if is_doc:
                before = re.sub(r"(?:^|\s)doc\s*$", " ", before)
            if is_doc and not before.strip() and not owner.doc:
                owner.doc = body
            elif not is_doc and not before.strip() and owner.kind == "file" and not owner.doc:
                # The banner at the top of each file: a useful synopsis.
                owner.doc = body
            text = before + text[match.end() :]

    # -- declarations -----------------------------------------------------

    def _declare(self, header: str, parent: Element, line: int) -> Element:
        header = self._take_comments(header, parent)
        element = self._parse_header(header, parent, line)
        parent.children.append(element)
        if re.match(r"^\s*(?:first|then)\b", header):
            step = element.name or element.typed_by
            if step:
                parent.flow.append(step)
        self._succession(" ".join(header.split()), parent)
        return element

    def _statement(self, statement: str, parent: Element, line: int) -> None:
        statement = self._take_comments(statement, parent).strip()
        if not statement:
            return
        tokens = statement.split()
        head = tokens[0]

        if head in ("first", "then", "if", "else"):
            self._succession(" ".join(statement.split()), parent)

        if head in ("first", "then"):
            rest = " ".join(tokens[1:]).split(":")[0].strip()
            words = rest.split()
            if words:
                parent.flow.append(words[-1])
            if len(words) < 2:
                return

        # `private import X::*` puts a visibility word in front of the head.
        if head in ("private", "public", "protected") and len(tokens) > 1:
            head, statement = tokens[1], " ".join(tokens[1:])

        if head in RELATION_HEADS:
            self._relation(head, statement, parent, line)
            return

        if head == "transition":
            transition = _TRANSITION.match(" ".join(statement.split()))
            if transition:
                name, source, trigger, target = transition.groups()
                parent.children.append(
                    Element(
                        kind="transition",
                        name=name or "",
                        flow=[source, target],
                        value=" ".join((trigger or "").split()),
                        parent=parent,
                        source=self.source,
                        line=line,
                        raw=" ".join(statement.split()),
                    )
                )
                return

        if head in CLAUSE_HEADS or (len(tokens) > 1 and tokens[1] in CLAUSE_HEADS):
            parent.children.append(
                Element(
                    kind="clause",
                    parent=parent,
                    source=self.source,
                    line=line,
                    raw=" ".join(statement.split()),
                )
            )
            return

        if parent.kind == "enum" and _IDENTIFIER.fullmatch(statement):
            parent.children.append(
                Element(
                    kind="enum-literal",
                    name=statement,
                    parent=parent,
                    source=self.source,
                    line=line,
                    raw=statement,
                )
            )
            return

        parent.children.append(self._parse_header(statement, parent, line))

    def _succession(self, text: str, parent: Element) -> None:
        """Record one step following another, in the body it was written in.

        The body is walked in source order, so "whatever came before" is simply
        the last step recorded for this parent -- which is why the state is
        keyed on the parent and not on the file.
        """
        key = id(parent)

        match = _FIRST_THEN.match(text)
        if match:
            source, target = match.group(1), match.group(2)
            parent.successions.append((source, target, ""))
            self.previous[key] = target
            return

        match = _FIRST.match(text)
        if match:
            self.previous[key] = match.group(1)
            return

        match = _THEN.match(text)
        if match:
            target = match.group(1)
            source = self.previous.get(key, "")
            if source and source != target:
                parent.successions.append((source, target, ""))
            self.previous[key] = target
            if target == "decide":
                self.branch[key] = target
            return

        match = _IF_THEN.match(text)
        if match:
            source = self.branch.get(key) or self.previous.get(key, "")
            if source:
                parent.successions.append((source, match.group(2), match.group(1)))
                self.branch[key] = source
            return

        match = _ELSE.match(text)
        if match:
            source = self.branch.get(key) or self.previous.get(key, "")
            if source:
                parent.successions.append((source, match.group(1), "else"))

    def _relation(self, head: str, statement: str, parent: Element, line: int) -> None:
        body = statement[len(head) :].strip()
        source = ""
        targets: list[str] = []
        if head == "dependency":
            match = re.match(r"(?:<[^>]*>\s*)?from\s+(.*?)\s+to\s+(.*)$", body, re.S)
            if match:
                source = match.group(1).strip()
                targets = split_top_level(match.group(2))
        elif head == "satisfy":
            match = re.match(r"(.*?)\s+by\s+(.*)$", body, re.S)
            if match:
                source = match.group(1).strip()
                targets = split_top_level(match.group(2))
            else:
                source = body
        elif head in ("allocate", "connect"):
            match = re.match(r"(.*?)\s+to\s+(.*)$", body, re.S)
            if match:
                source = match.group(1).strip()
                targets = split_top_level(match.group(2))
        else:
            source = parent.qualified_name
            targets = split_top_level(body)
        self.relations.append(
            Relation(
                kind=head,
                source=" ".join(source.split()),
                targets=[" ".join(target.split()) for target in targets],
                origin=parent.qualified_name,
                file=self.source,
                line=line,
            )
        )

    # -- the header tokeniser ---------------------------------------------

    def _parse_header(self, header: str, parent: Element, line: int) -> Element:
        element = Element(parent=parent, source=self.source, line=line, raw=" ".join(header.split()))
        text = header

        stage = _STAGE.search(text)
        if stage:
            element.stage = int(stage.group(1))
            text = _STAGE.sub(" ", text)
        deferred = _DEFERRED.search(text)
        if deferred:
            element.deferred_reason = " ".join(deferred.group(1).split())
            text = _DEFERRED.sub(" ", text)

        element.keywords.extend(re.findall(r"#(\w+)", text))
        text = re.sub(r"#\w+", " ", text)

        short = _SHORT_NAME.search(text)
        if short:
            element.short_name = short.group(1)
            text = _SHORT_NAME.sub(" ", text)

        # Split the value off first: it may hold colons, brackets and commas.
        value_match = re.search(r"(?<![:>=!<])=(?!=)", text)
        if value_match:
            element.value = " ".join(text[value_match.end() :].split())
            text = text[: value_match.start()]

        multiplicity = _MULTIPLICITY.search(text)
        if multiplicity:
            element.multiplicity = multiplicity.group(1).strip()
            text = _MULTIPLICITY.sub(" ", text)

        for operator, field in ((":>>", "redefines"), (":>", "specializes")):
            index = text.find(operator)
            if index != -1:
                setattr(element, field, _clean_type(text[index + len(operator) :]))
                text = text[:index]
                break
        else:
            index = text.find(":")
            if index != -1:
                element.typed_by = _clean_type(text[index + 1 :])
                text = text[:index]

        tokens = [token for token in re.split(r"[\s,]+", text.strip()) if token]
        while tokens and tokens[0] in MODIFIERS:
            element.modifiers.append(tokens.pop(0))
        if tokens and tokens[0] in KINDS:
            element.kind = tokens.pop(0)
        if tokens and tokens[0] == "def":
            element.is_definition = True
            tokens.pop(0)
        if tokens and _IDENTIFIER.fullmatch(tokens[0]):
            element.name = tokens.pop(0)

        if not element.kind:
            annotation = re.match(r"@(\w+)\b", element.raw)
            if annotation:
                element.kind = "annotation"
                element.name = annotation.group(1)
            elif element.name or element.redefines:
                # A redefinition such as `attribute :>> status = ...` whose
                # kind was consumed, or a flow step such as `then done`.
                element.kind = "feature"
            else:
                self.unparsed.append(element)
        return element

    def _fold_annotations(self, element: Element) -> None:
        """Move `@stage` / `@deferred` bodies onto the element they annotate."""
        for child in list(element.children):
            if child.kind != "annotation":
                continue
            if child.name == "stage":
                digits = re.search(r"\d+", child.attribute_value("number"))
                if digits:
                    element.stage = int(digits.group(0))
            elif child.name == "deferred":
                reason = child.attribute_value("reason").strip().strip('"')
                if reason:
                    element.deferred_reason = " ".join(reason.split())
            element.children.remove(child)


def _clean_type(text: str) -> str:
    """Normalise a type reference, dropping trailing feature words."""
    words = [word for word in text.replace(",", ", ").split() if word not in TYPE_TAIL]
    return " ".join(words).replace(" ,", ",")


def parse_text(source: str, text: str) -> tuple[Element, list[Relation], list[Element]]:
    """Parse one file. Returns its root, its relations, and its unparsed lines."""
    parser = _Parser(source, text)
    root = parser.run()
    _reparent(root)
    return root, parser.relations, parser.unparsed


def _reparent(element: Element) -> None:
    for child in element.children:
        child.parent = element
        _reparent(child)


def load(paths: list[pathlib.Path], root: pathlib.Path) -> Model:
    """Load every file as one model, in the order given.

    The order is the caller's, and `gen-arch-doc.py` sorts by filename -- which
    is why the packages are numbered: `00-lifecycle` defines the keywords
    every other file then carries.
    """
    model = Model()
    hasher = hashlib.sha256()
    for path in sorted(paths):
        text = path.read_text(encoding="utf-8")
        relative = path.relative_to(root).as_posix()
        hasher.update(relative.encode("utf-8"))
        hasher.update(b"\0")
        hasher.update(text.encode("utf-8"))
        file_root, relations, unparsed = parse_text(relative, text)
        model.files.append(relative)
        model.relations.extend(relations)
        model.unparsed.extend(unparsed)
        model.synopses[relative] = file_root.doc
        for child in file_root.children:
            child.parent = None
            model.roots.append(child)
    model.digest = hasher.hexdigest()
    return model
