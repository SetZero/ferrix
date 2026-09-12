"""Build the architecture document from the model. The extension point.

Everything the generated document says is decided here, and nothing here knows
what Markdown or HTML is. A section is a function that takes the `Model` and
the `Doc` and appends blocks; `SECTIONS` at the bottom is the running order.
Adding to the document is appending a function to that list -- Markdown, HTML
and the table of contents all gain it, and no renderer changes.

Two rules keep a section honest:

  * **Say what the model says.** Prose that is not in the model, and not a
    caption explaining how to read a table, does not belong in a generated
    document; it belongs in `docs/ARCHITECTURE.md`, which is the source of
    truth this indexes.
  * **Never invent a count.** Numbers come from walking the model. A figure
    written in by hand is a figure that will be wrong in two commits, and a
    generated document is exactly where nobody will think to check it.
"""

from __future__ import annotations

import re

from . import diagrams
from .document import (
    Bullets,
    Code,
    Defs,
    Diagram,
    Doc,
    Heading,
    Note,
    P,
    Steps,
    Table,
    Tree,
    a,
    b,
    c,
    i,
    t,
)
from .model import MATURITIES, MATURITY_LABEL, Element, Model, humanise

# Packages whose contents get a generic section. The rest are rendered by a
# section of their own, above, because their shape carries meaning a generic
# dump would lose.
SUBSYSTEM_PACKAGES = (
    ("FerrixMemory", "Memory"),
    ("FerrixScheduling", "Processors, time and scheduling"),
    ("FerrixObjects", "Kernel objects and the two ABIs"),
    ("FerrixIsolation", "Isolation"),
    ("FerrixDrivers", "Devices and drivers"),
    ("FerrixStorage", "Storage"),
)


# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------


def place(doc: Doc, *figures) -> None:
    """Put a figure in the document, numbered, or nothing if there is none.

    Every builder in `diagrams.py` returns `None` when the model does not hold
    enough to draw, so a section asks for a figure unconditionally and the
    document simply does not gain one. Numbering happens here because it is the
    order of placement, not of construction, that a caption refers to.
    """
    placed = doc.meta.setdefault("figures", [])
    for figure in figures:
        if figure is None or figure.is_empty:
            continue
        block = Diagram(
            figure=figure,
            number=len(placed) + 1,
            file=f"diagrams/{figure.name}.svg",
        )
        placed.append(block)
        doc.add(block)


def maturity_run(element: Element):
    """The element's maturity as a code run the HTML renderer will colour."""
    if element.is_deferred:
        return c("@deferred")
    if element.maturity:
        return c("#" + element.maturity)
    return t("—")


def stage_text(element: Element) -> str:
    return str(element.stage) if element.stage is not None else "—"


_CODE_SPAN = re.compile(r"`([^`\n]+)`")

# A full stop that ends a sentence: followed by space and a capital or a digit,
# and not one of the abbreviations the model's prose is full of.
_SENTENCE_END = re.compile(r"(?<=[.!?])\s+(?=[A-Z0-9§])")
_ABBREVIATION = re.compile(r"(?:\b[A-Za-z]|§\s*\d+|\bvs|\be\.g|\bi\.e|\bNo)\.$")


def prose(text: str) -> list:
    """Model prose as runs, honouring the backtick spans the model writes.

    Doc comments in the model mark code with backticks -- `libs/sched`,
    `#[expect]`, `rustc` -- and mean it. Turning those into real code spans is
    the one interpretation of model text this generator makes, and it is safe
    because an unbalanced backtick simply fails to match and stays literal.
    """
    out: list = []
    position = 0
    for match in _CODE_SPAN.finditer(text):
        if match.start() > position:
            out.append(t(text[position : match.start()]))
        out.append(c(match.group(1)))
        position = match.end()
    if position < len(text):
        out.append(t(text[position:]))
    return out or [t(text)]


def first_sentence(text: str, limit: int = 260) -> list:
    """The opening sentence of a doc comment, as runs, for a table cell."""
    if not text:
        return []
    paragraph = " ".join(text.split("\n\n")[0].split())
    sentence = paragraph
    for match in _SENTENCE_END.finditer(paragraph):
        candidate = paragraph[: match.start()]
        if _ABBREVIATION.search(candidate):
            continue
        sentence = candidate
        break
    if len(sentence) > limit:
        sentence = sentence[: limit - 1].rsplit(" ", 1)[0] + "…"
    return prose(sentence)


def paragraphs(doc: Doc, text: str) -> None:
    for paragraph in text.split("\n\n"):
        if paragraph.strip():
            doc.add(P(prose(paragraph.strip())))


def unquote(value: str) -> str:
    return value.strip().strip('"')


def enum_label(value: str) -> str:
    """`StageStatus::Done` -> `Done`."""
    return value.split("::")[-1].strip() if value else ""


def definitions(package: Element) -> list[Element]:
    """Definitions declared directly in a package, in source order."""
    return [
        child
        for child in package.children
        if child.is_definition or (child.kind in ("part", "requirement") and child.typed_by)
    ]


# ---------------------------------------------------------------------------
# Sections
# ---------------------------------------------------------------------------


def section_about(model: Model, doc: Doc) -> None:
    doc.add(Heading(1, "About this document"))
    doc.add(
        P(
            [
                t("This is generated from the SysML v2 model in "),
                c("docs/sysml/"),
                t(
                    ", which is itself an index over the prose. The prose is the "
                    "source of truth: "
                ),
                c("docs/ARCHITECTURE.md"),
                t(" says what is being built and "),
                c("docs/ROADMAP.md"),
                t(
                    " in what order. What the model adds, and what this document "
                    "is therefore able to state without a human keeping count, is "
                    "that every element carries a maturity keyword — so nothing "
                    "here confuses what runs today with what the roadmap still owes."
                ),
            ]
        )
    )

    counts = model.maturity_counts()
    total = sum(1 for _ in model.walk())
    doc.add(
        Table(
            head=["Package", "File", "What it holds"],
            rows=[
                [
                    c(root.name),
                    c(path.rsplit("/", 1)[-1]),
                    prose(_synopsis_body(model.synopses.get(path, ""))),
                ]
                for path, root in _files_and_roots(model)
            ],
            caption=[
                t(f"{len(model.files)} files, {len(model.packages())} packages, "),
                t(f"{total} elements, {len(model.relations)} relations. "),
                t("Model digest "),
                c(model.digest[:16]),
                t("."),
            ],
        )
    )

    doc.add(
        Table(
            head=["Maturity", "Elements", "Meaning"],
            align=["left", "right", "left"],
            rows=[
                [c("#implemented"), str(counts["implemented"]), _MATURITY_PROSE["implemented"]],
                [c("#inProgress"), str(counts["inProgress"]), _MATURITY_PROSE["inProgress"]],
                [
                    c("#writtenAhead"),
                    str(counts["writtenAhead"]),
                    _MATURITY_PROSE["writtenAhead"],
                ],
                [c("#planned"), str(counts["planned"]), _MATURITY_PROSE["planned"]],
                [c("@deferred"), str(len(model.deferred())), _MATURITY_PROSE["deferred"]],
            ],
            caption=[
                t(
                    "An element carries its own keyword or none; a keyword is never "
                    "inherited from a parent, so a "
                ),
                c("#planned"),
                t(" field inside an "),
                c("#implemented"),
                t(" part still reads as planned."),
            ],
        )
    )

    if model.unparsed:
        doc.add(
            Note(
                f"{len(model.unparsed)} declaration(s) in the model were not "
                "recognised by the reader and are missing from this document. "
                "They are listed in model.json under \"unparsed\".",
                label="Incomplete",
            )
        )


_MATURITY_PROSE = {
    "implemented": "The code exists and the QEMU boot test exercises it on every architecture it applies to.",
    "inProgress": "The owning stage has started; part of the element runs.",
    "writtenAhead": "A libs/ crate exists and passes its host tests, but nothing in kernel/ calls it yet.",
    "planned": "Only the design exists, in docs/ARCHITECTURE.md. Nothing stands in for it.",
    "deferred": "Work a finished stage explicitly left behind, carrying the reason that stage gave.",
}


def _files_and_roots(model: Model):
    pairs = []
    for path in model.files:
        for root in model.roots:
            if root.source == path:
                pairs.append((path, root))
    return pairs


def _synopsis_body(synopsis: str) -> str:
    """Drop the `Ferrix — SysML v2 model, part N: x.` title line."""
    parts = synopsis.split("\n\n")
    return parts[1] if len(parts) > 1 else synopsis


def section_requirements(model: Model, doc: Doc) -> None:
    package = model.package("FerrixRequirements")
    if package is None:
        return
    doc.add(Heading(1, "Requirements"))

    goal = model.by_short_name("G")
    if goal is not None:
        doc.add(Heading(2, "The goal"))
        paragraphs(doc, goal.doc)
        rows = []
        for child in goal.children:
            if child.kind != "requirement":
                continue
            rows.append([c(child.short_name), humanise(child.name), prose(child.doc)])
        if rows:
            doc.add(Table(head=["Id", "Requirement", "What it forces"], rows=rows))

    place(doc, diagrams.goals(model))

    extra = model.by_short_name("G+")
    if extra is not None:
        doc.add(Note([c("G+"), t(" — "), *prose(extra.doc)], label=humanise(extra.name)))

    principles = model.find("FerrixRequirements::Principles")
    if principles is not None:
        doc.add(Heading(2, "Design rules"))
        paragraphs(doc, principles.doc)
        doc.add(
            Table(
                head=["Id", "Rule", "Why"],
                rows=[
                    [c(child.short_name), humanise(child.name), prose(child.doc)]
                    for child in principles.children
                    if child.short_name
                ],
            )
        )

    non_promises = model.find("FerrixRequirements::NonPromises")
    if non_promises is not None:
        doc.add(Heading(2, "Promises deliberately not made"))
        doc.add(
            Defs(
                [
                    ([c(child.short_name), t(" "), b(humanise(child.name))], prose(child.doc))
                    for child in non_promises.children
                    if child.short_name
                ]
            )
        )


def section_structure(model: Model, doc: Doc) -> None:
    package = model.package("FerrixStructure")
    if package is None:
        return
    doc.add(Heading(1, "Structure"))

    system = package.child("Ferrix")
    if system is not None:
        paragraphs(doc, system.doc)
        doc.add(Tree(_part_tree(system, model, 0, 3)))
    place(
        doc,
        diagrams.connections(model, "FerrixStructure::Deployment", "FerrixStructure::Ferrix"),
    )

    machine = package.child("Machine")
    if machine is not None:
        doc.add(Heading(2, "The machine"))
        paragraphs(doc, machine.doc)
        doc.add(_feature_table(machine))

    ports = [child for child in package.children if child.kind == "port" and child.is_definition]
    if ports:
        doc.add(Heading(2, "Interfaces between the big pieces"))
        doc.add(
            Defs([([c(port.name)], prose(port.doc) if port.doc else humanise(port.name)) for port in ports])
        )

    for name, heading in (("Loader", "The loader"), ("Kernel", "The kernel")):
        element = package.child(name)
        if element is None:
            continue
        doc.add(Heading(2, heading))
        paragraphs(doc, element.doc)
        rows = []
        for child in element.children:
            if child.kind not in ("part", "port"):
                continue
            rows.append(
                [
                    c(child.name),
                    c(child.typed_by) if child.typed_by else "",
                    maturity_run(child),
                    stage_text(child),
                    first_sentence(child.doc),
                ]
            )
        if rows:
            doc.add(
                Table(
                    head=["Feature", "Type", "Maturity", "Stage", "Note"],
                    align=["left", "left", "left", "right", "left"],
                    rows=rows,
                )
            )
        place(doc, diagrams.composition(model, element.qualified_name))
        for constraint in element.of_kind("constraint"):
            if constraint.doc:
                doc.add(Note(prose(constraint.doc), label=humanise(constraint.name) or "Constraint"))


def _part_tree(element: Element, model: Model, depth: int, limit: int) -> list:
    """The composition tree under a part, following types one hop at a time."""
    lines: list = []
    if depth > limit:
        return lines
    for child in element.children:
        if child.kind != "part":
            continue
        label = child.name or child.typed_by
        suffix = f" : {child.typed_by}" if child.typed_by and child.name else ""
        mark = ""
        if child.is_deferred:
            mark = "  [deferred]"
        elif child.maturity:
            mark = f"  [{MATURITY_LABEL[child.maturity]}]"
        lines.append((depth, f"{label}{suffix}{mark}"))
        target = model.resolve(child.typed_by) if child.typed_by else None
        if target is not None and target is not child and depth < limit:
            lines.extend(_part_tree(target, model, depth + 1, limit))
    return lines


def _feature_table(element: Element) -> Table:
    rows = []
    for child in element.children:
        if child.kind not in ("part", "attribute", "port", "item", "ref"):
            continue
        name = child.name or child.redefines
        if not name:
            continue
        rows.append(
            [
                c(name),
                c(child.typed_by) if child.typed_by else "",
                c(child.multiplicity) if child.multiplicity else "",
                first_sentence(child.doc),
            ]
        )
    return Table(head=["Feature", "Type", "Multiplicity", "Note"], rows=rows)


def section_architectures(model: Model, doc: Doc) -> None:
    package = model.package("FerrixStructure")
    if package is None:
        return
    facade = package.child("ArchFacade")
    layer = package.child("ArchLayer")
    if facade is None:
        return

    doc.add(Heading(1, "The architecture facade"))
    paragraphs(doc, facade.doc)
    place(doc, *diagrams.generalisations(model, "FerrixStructure"))

    variants = []
    if layer is not None:
        for variant in layer.children:
            target = model.resolve(variant.typed_by)
            if target is not None:
                variants.append(target)
    if variants:
        rows = [
            [
                c(unquote(variant.attribute_value("NAME"))),
                c(variant.name),
                maturity_run(variant),
                unquote(variant.attribute_value("TLB_FLUSH_IS_BROADCAST")) or "—",
                first_sentence(variant.doc),
            ]
            for variant in variants
        ]
        doc.add(
            Table(
                head=["Target", "Definition", "Maturity", "TLB flush broadcasts", "Notes"],
                rows=rows,
                caption=prose(layer.doc) if layer.doc else None,
            )
        )

        for variant in variants:
            doc.add(Heading(2, f"{unquote(variant.attribute_value('NAME')) or variant.name}"))
            paragraphs(doc, variant.doc)
            rows = []
            for child in variant.children:
                if child.kind != "part":
                    continue
                rows.append(
                    [
                        c(child.name),
                        c(child.typed_by) if child.typed_by else "",
                        maturity_run(child),
                        prose(child.deferred_reason)
                        if child.is_deferred
                        else first_sentence(child.doc),
                    ]
                )
            if rows:
                doc.add(Table(head=["Part", "Type", "Maturity", "Note"], rows=rows))

    doc.add(Heading(2, "What the facade exports"))
    doc.add(
        P(
            "Every architecture supplies each of these; generic kernel code reaches "
            "the CPU through nothing else."
        )
    )
    actions = [child for child in facade.children if child.kind == "action"]
    doc.add(
        Table(
            head=["Operation", "Maturity", "Stage", "Note"],
            align=["left", "left", "right", "left"],
            rows=[
                [c(action.name), maturity_run(action), stage_text(action), first_sentence(action.doc)]
                for action in actions
            ],
        )
    )

    shared = package.child("SharedArmDrivers")
    if shared is not None:
        doc.add(Heading(2, "Drivers shared by the Arm pair"))
        paragraphs(doc, shared.doc)
        doc.add(
            Defs(
                [
                    ([c(child.name), t(" : "), c(child.typed_by)], prose(child.doc))
                    for child in shared.children
                    if child.kind == "part"
                ]
            )
        )


def section_boot(model: Model, doc: Doc) -> None:
    package = model.package("FerrixBoot")
    if package is None:
        return
    doc.add(Heading(1, "Boot"))
    paragraphs(doc, _synopsis_body(model.synopses.get("docs/sysml/03-boot.sysml", "")))

    handoff = package.child("BootInfo")
    if handoff is not None:
        doc.add(Heading(2, "The hand-off"))
        paragraphs(doc, handoff.doc)
        rows = []
        for child in handoff.children:
            name = child.name or child.redefines
            if not name or child.kind not in ("attribute", "item", "part"):
                continue
            rows.append(
                [
                    c(name),
                    c(child.typed_by) if child.typed_by else "",
                    c(unquote(child.value)) if child.value else "",
                    first_sentence(child.doc),
                ]
            )
        doc.add(Table(head=["Field", "Type", "Value", "Note"], rows=rows))

    layouts = [
        child
        for child in package.children
        if child.kind == "part" and child.typed_by == "VirtualLayout"
    ]
    if layouts:
        doc.add(Heading(2, "Address layouts"))
        definition = package.child("VirtualLayout")
        if definition is not None:
            paragraphs(doc, definition.doc)
        for layout in layouts:
            bits = layout.attribute_value("addressBits")
            doc.add(Heading(3, f"{humanise(layout.name)} — {bits}-bit"))
            paragraphs(doc, layout.doc)
            rows = []
            for field in ("image", "physmap", "vmap", "user"):
                value = layout.attribute_value(field)
                base, limit, purpose = _address_range(value)
                if base:
                    rows.append([c(field), c(base), c(limit), purpose])
            reserved = layout.attribute_value("vmapReserved")
            doc.add(
                Table(
                    head=["Range", "Base", "Limit", "Purpose"],
                    rows=rows,
                    caption=(
                        [t("vmap reserved for fixed windows: "), c(unquote(reserved))]
                        if reserved
                        else None
                    ),
                )
            )

    for name, heading in (
        ("LoaderSequence", "The loader's sequence"),
        ("KernelBringUp", "The kernel's bring-up"),
    ):
        action = package.child(name)
        if action is None:
            continue
        doc.add(Heading(2, heading))
        paragraphs(doc, action.doc)
        doc.add(Steps(_flow_steps(action)))
        place(doc, diagrams.flow(model, action.qualified_name))

    checks = [
        child
        for child in package.children
        if child.kind == "action" and child.is_definition and child.name.endswith("Check")
    ]
    checks += [child for child in package.children if child.name in ("Stage4BringUp", "FinishMemory")]
    if checks:
        doc.add(Heading(2, "The self-checks each boot runs"))
        doc.add(
            P(
                "Every stage's exit criterion runs in kmain on every boot. Each has its "
                "own panic line, so a failing boot test names a reason rather than a timeout."
            )
        )
        for check in checks:
            doc.add(Heading(3, humanise(check.name)))
            if check.stage is not None:
                doc.add(P([b("Stage "), b(str(check.stage))]))
            paragraphs(doc, check.doc)
            doc.add(
                Defs(
                    [
                        ([c(step.name)], prose(step.doc) if step.doc else humanise(step.name))
                        for step in check.children
                        if step.kind == "action" and step.name
                    ]
                )
            )

    dispatch = package.child("TrapDispatch")
    if dispatch is not None:
        doc.add(Heading(2, "Traps"))
        paragraphs(doc, dispatch.doc)
        place(
            doc,
            diagrams.flow(model, "FerrixBoot::Dispatch"),
            diagrams.flow(model, "FerrixBoot::HandlePageFault"),
        )
        kinds = package.child("TrapKind")
        if kinds is not None:
            doc.add(
                P(
                    [
                        t("Classified into terms all three architectures share: "),
                        *_joined_codes([literal.name for literal in kinds.children]),
                        t(". "),
                        *prose(kinds.doc),
                    ]
                )
            )


def _address_range(value: str) -> tuple[str, str, str]:
    base = re.search(r'base\s*=\s*"([^"]*)"', value or "")
    limit = re.search(r'limit\s*=\s*"([^"]*)"', value or "")
    purpose = re.search(r'purpose\s*=\s*"([^"]*)"', value or "")
    return (
        base.group(1) if base else "",
        limit.group(1) if limit else "",
        purpose.group(1) if purpose else "",
    )


def _flow_steps(action: Element) -> list:
    """The action's `first`/`then` order, with each step's doc beside it."""
    steps: list = []
    for name in action.flow:
        if name in ("start", "done"):
            continue
        child = action.child(name)
        note = child.doc if child is not None else ""
        entry = [c(name)]
        if note:
            entry.extend([t(" — "), *prose(note)])
        steps.append(entry)
    return steps


def _joined_codes(names: list[str]) -> list:
    out: list = []
    for index, name in enumerate(names):
        if index:
            out.append(t(", " if index < len(names) - 1 else " and "))
        out.append(c(name))
    return out


def section_subsystems(model: Model, doc: Doc) -> None:
    doc.add(Heading(1, "Subsystems"))
    doc.add(
        P(
            "One entry per definition, in the order the model declares it, with the "
            "maturity keyword it carries and the stage that owns it."
        )
    )
    for package_name, heading in SUBSYSTEM_PACKAGES:
        package = model.package(package_name)
        if package is None:
            continue
        doc.add(Heading(2, heading))
        paragraphs(doc, _synopsis_body(_package_synopsis(model, package)))
        # Figures before the entries: what the package is shaped like, then
        # what it declares. Each builder decides for itself whether the package
        # holds enough to draw, so adding a subsystem adds its diagrams too.
        place(
            doc,
            *diagrams.generalisations(model, package_name),
            *diagrams.flows(model, package_name),
            *diagrams.state_machines(model, package_name),
        )
        for element in package.children:
            if element.kind in ("enum",):
                doc.add(
                    P(
                        [
                            b(element.name),
                            t(" — "),
                            *_joined_codes([lit.name for lit in element.children if lit.name]),
                            t(". "),
                            *prose(element.doc),
                        ]
                    )
                )
                continue
            if not element.is_definition or not element.name:
                continue
            doc.add(Heading(3, element.name))
            marks = [maturity_run(element)]
            if element.stage is not None:
                marks.extend([t("  ·  stage "), t(str(element.stage))])
            if element.specializes:
                marks.extend([t("  ·  specialises "), c(element.specializes)])
            doc.add(P(marks))
            if element.is_deferred:
                doc.add(Note(prose(element.deferred_reason), label="Deferred"))
            paragraphs(doc, element.doc)
            rows = []
            for child in element.children:
                name = child.name or child.redefines
                if not name or child.kind not in ("part", "attribute", "action", "port", "item"):
                    continue
                detail = (
                    prose(child.deferred_reason)
                    if child.is_deferred
                    else first_sentence(child.doc)
                )
                rows.append(
                    [
                        c(name),
                        child.kind,
                        c(child.typed_by) if child.typed_by else "",
                        maturity_run(child) if (child.maturity or child.is_deferred) else "",
                        detail,
                    ]
                )
            if rows:
                doc.add(
                    Table(head=["Feature", "Kind", "Type", "Maturity", "Note"], rows=rows)
                )
            if element.flow:
                doc.add(Steps(_flow_steps(element)))


def _package_synopsis(model: Model, package: Element) -> str:
    for path, root in _files_and_roots(model):
        if root is package:
            return model.synopses.get(path, "")
    return ""


def section_workspace(model: Model, doc: Doc) -> None:
    workspace = model.find("FerrixStructure::Workspace")
    if workspace is None:
        return
    doc.add(Heading(1, "The workspace"))
    paragraphs(doc, workspace.doc)

    rows = []
    for crate in workspace.children:
        if crate.kind != "part" or not crate.typed_by:
            continue
        path = unquote(crate.attribute_value("path"))
        if not path:
            continue
        forbids = unquote(crate.attribute_value("forbidsUnsafe"))
        tests = unquote(crate.attribute_value("hostTests"))
        rows.append(
            [
                c(path),
                maturity_run(crate),
                stage_text(crate),
                c("forbid") if forbids == "true" else ("allowed" if forbids else ""),
                tests or "—",
                first_sentence(crate.doc),
            ]
        )
    doc.add(
        Table(
            head=["Crate", "Maturity", "Stage", "Unsafe", "Host tests", "Note"],
            align=["left", "left", "right", "left", "right", "left"],
            rows=rows,
        )
    )

    edges = [
        relation
        for relation in model.relations_of("dependency")
        if relation.origin.endswith("Workspace")
    ]
    if edges:
        doc.add(Heading(2, "Dependency edges"))
        place(
            doc,
            diagrams.dependencies(
                model,
                "FerrixStructure::Workspace",
                title="The crate graph",
                name="crate-dependencies",
                sublabel_attribute="path",
                include_members=True,
            ),
        )
        doc.add(
            Bullets(
                [
                    [c(relation.source), t(" → "), *_joined_codes(relation.targets)]
                    for relation in edges
                ]
            )
        )

    for name, label in (
        ("kernelTargets", "Kernel targets"),
        ("loaderTargets", "Loader targets"),
        ("toolchain", "Toolchain"),
    ):
        value = workspace.attribute_value(name)
        if value:
            items = [unquote(part) for part in re.findall(r'"([^"]*)"', value)] or [unquote(value)]
            doc.add(P([b(label), t(": "), *_joined_codes(items), t(".")]))


def section_roadmap(model: Model, doc: Doc) -> None:
    package = model.package("FerrixRoadmap")
    if package is None:
        return
    doc.add(Heading(1, "Roadmap"))
    definition = package.child("Stage")
    if definition is not None:
        paragraphs(doc, definition.doc)
    place(doc, diagrams.stages(model))

    stages = [child for child in package.children if child.typed_by == "Stage"]
    doc.add(
        Table(
            head=["Id", "No.", "Stage", "Status", "Size", "Maturity"],
            align=["left", "right", "left", "left", "left", "left"],
            rows=[
                [
                    c(stage.short_name),
                    stage.attribute_value("number") or "—",
                    humanise(stage.name),
                    enum_label(stage.attribute_value("status")),
                    unquote(stage.attribute_value("size")),
                    maturity_run(stage),
                ]
                for stage in stages
            ],
            caption="Sizes are order-of-magnitude and not a schedule.",
        )
    )

    satisfies: dict[str, list[str]] = {}
    for relation in model.relations_of("satisfy"):
        satisfies.setdefault(relation.source, []).extend(relation.targets)
    allocates: dict[str, list[str]] = {}
    for relation in model.relations_of("allocate"):
        allocates.setdefault(relation.source, []).extend(relation.targets)
    verified: dict[str, list[str]] = {}
    for relation in model.relations_of("verify"):
        for target in relation.targets:
            verified.setdefault(target, []).append(relation.origin)

    for stage in stages:
        doc.add(Heading(2, f"{stage.short_name} — {humanise(stage.name)}"))
        facts = [
            b(enum_label(stage.attribute_value("status"))),
            t("  ·  size "),
            t(unquote(stage.attribute_value("size"))),
            t("  ·  "),
            maturity_run(stage),
        ]
        doc.add(P(facts))
        paragraphs(doc, stage.doc)

        if stage.name in satisfies:
            doc.add(P([b("Satisfied by: "), *_joined_codes(satisfies[stage.name])]))
        if stage.name in allocates:
            doc.add(P([b("Allocated to: "), *_joined_codes(allocates[stage.name])]))
        verifiers = verified.get(stage.name, [])
        if verifiers:
            doc.add(P([b("Verified by: "), *_joined_codes(sorted(set(verifiers)))]))

        deferrals = [child for child in stage.children if child.is_deferred]
        if deferrals:
            doc.add(
                Defs(
                    [([c(child.name)], prose(child.deferred_reason)) for child in deferrals]
                )
            )

    ordering = [
        relation
        for relation in model.relations_of("dependency")
        if relation.origin.endswith("FerrixRoadmap")
    ]
    if ordering:
        doc.add(Heading(2, "Ordering"))
        doc.add(
            P(
                "Every stage ends in something that runs, and nothing is stubbed that a "
                "later stage has to unpick. These are the edges the model draws."
            )
        )
        doc.add(
            Bullets(
                [
                    [c(relation.source), t(" depends on "), *_joined_codes(relation.targets)]
                    for relation in ordering
                ]
            )
        )


def section_assurance(model: Model, doc: Doc) -> None:
    package = model.package("FerrixAssurance")
    if package is None:
        return
    doc.add(Heading(1, "Assurance"))
    definition = package.child("Gate")
    if definition is not None:
        paragraphs(doc, definition.doc)

    place(
        doc,
        diagrams.trace(
            model,
            ("verify",),
            name="gates-and-rules",
            title="The gates and the rules they uphold",
            caption=(
                "Each gate, and the design rule it exists to enforce. A rule with no gate "
                "into it is a rule enforced by review."
            ),
            origin_prefix="FerrixAssurance",
            source_kind="test",
            target_kind="requirement",
        ),
    )

    verified: dict[str, list[str]] = {}
    for relation in model.relations_of("verify"):
        verified.setdefault(relation.origin, []).extend(relation.targets)

    gates = [child for child in package.children if child.typed_by == "Gate"]
    doc.add(
        Table(
            head=["Gate", "Command", "Upholds", "Note"],
            rows=[
                [
                    c(gate.name),
                    c(unquote(gate.attribute_value("command"))),
                    _joined_codes(verified.get(gate.qualified_name, [])) or "—",
                    first_sentence(gate.doc),
                ]
                for gate in gates
            ],
            caption=f"{len(gates)} gates, cheapest first.",
        )
    )

    budget = package.child("AssemblyBudget")
    if budget is not None:
        doc.add(Heading(2, "The assembly budget"))
        paragraphs(doc, budget.doc)
        sites = budget.attribute_value("sites")
        rows = [
            [c(file), lines]
            for file, lines in re.findall(r'file\s*=\s*"([^"]*)"\s*,\s*maxLines\s*=\s*(\d+)', sites)
        ]
        if rows:
            doc.add(
                Table(
                    head=["Site", "Line budget"],
                    align=["left", "right"],
                    rows=rows,
                    caption=[
                        t("Total cap "),
                        c(budget.attribute_value("maxTotalLines")),
                        t(" lines; ratio backstop "),
                        c(budget.attribute_value("maxRatio")),
                        t(", target "),
                        c(budget.attribute_value("targetRatio")),
                        t("."),
                    ],
                )
            )

    reaches = [child for child in package.children if child.typed_by == "LayerReach"]
    if reaches:
        doc.add(Heading(2, "What each layer's tests can reach"))
        doc.add(
            Table(
                head=["Layer", "Reached by", "Note"],
                rows=[
                    [
                        c(unquote(reach.attribute_value("layer"))),
                        _joined_codes(
                            [enum_label(part) for part in re.findall(r"TestReach::(\w+)", reach.attribute_value("reach"))]
                        ),
                        first_sentence(reach.doc),
                    ]
                    for reach in reaches
                ],
            )
        )

    owed = [
        child
        for child in package.children
        if child.kind == "verification" and child.is_definition and child.maturity == "planned"
    ]
    if owed:
        doc.add(Heading(2, "Verification later stages owe"))
        doc.add(
            Defs(
                [
                    (
                        [c(child.name), t(f"  (stage {child.stage})" if child.stage else "")],
                        prose(child.doc),
                    )
                    for child in owed
                ]
            )
        )

    boot_tests = [
        child
        for child in model.package("FerrixRoadmap").children
        if child.kind == "verification" and child.typed_by == "BootTest"
    ] if model.package("FerrixRoadmap") else []
    if boot_tests:
        doc.add(Heading(2, "The boot tests"))
        definition = model.find("FerrixRoadmap::BootTest")
        if definition is not None:
            paragraphs(doc, definition.doc)
        verified_by = {}
        for relation in model.relations_of("verify"):
            verified_by.setdefault(relation.origin, []).extend(relation.targets)
        doc.add(
            Table(
                head=["Boot test", "Architecture", "Verifies"],
                rows=[
                    [
                        c(test.name),
                        enum_label(test.attribute_value("arch")),
                        _joined_codes(verified_by.get(test.qualified_name, [])),
                    ]
                    for test in boot_tests
                ],
            )
        )


def section_traceability(model: Model, doc: Doc) -> None:
    doc.add(Heading(1, "Traceability"))
    doc.add(
        P(
            "Every satisfy, allocate and verify edge the model draws, resolved against "
            "the element tree. A row whose target does not resolve is a broken reference "
            "and is marked."
        )
    )
    place(
        doc,
        diagrams.trace(
            model,
            ("satisfy", "allocate"),
            name="stages-and-parts",
            title="Stages and the parts that answer them",
            caption=(
                "Each line carries the word the model wrote: `satisfy` where the part "
                "exists, `allocate` where it is one the stage still owes."
            ),
        ),
        diagrams.trace(
            model,
            ("verify",),
            name="tests-and-stages",
            title="The boot tests and the stages they verify",
            caption=(
                "Each verification case, and every stage whose exit criterion it "
                "demonstrates on a boot."
            ),
            origin_prefix="FerrixRoadmap",
            source_kind="test",
        ),
    )

    for kind, heading, arrow in (
        ("satisfy", "Satisfied by", "is satisfied by"),
        ("allocate", "Allocated to", "is allocated to"),
        ("verify", "Verified by", "is verified by"),
    ):
        relations = model.relations_of(kind)
        if not relations:
            continue
        rows = []
        for relation in relations:
            if kind == "verify":
                for target in relation.targets:
                    rows.append([_ref(model, target), c(relation.origin or "—")])
            else:
                for target in relation.targets:
                    rows.append([_ref(model, relation.source), _ref(model, target)])
        doc.add(Heading(2, heading))
        doc.add(
            Table(
                head=["Requirement", "Element"],
                rows=rows,
                caption=f"{len(rows)} edges — each reads “requirement {arrow} element”.",
            )
        )

    doc.add(Heading(2, "Coverage"))
    requirements = [
        element
        for element in model.walk()
        if element.kind == "requirement" and element.short_name
    ]
    # Three different edges trace a requirement to something. A stage names
    # the parts that satisfy it; a future stage is allocated to the parts that
    # will; and the roadmap draws a dependency from a stage to the goal
    # requirement that stage discharges. Counting only the first two would
    # report G.1 to G.8 as untraced, which they are not.
    traced: dict[str, list[str]] = {}
    for kind in ("satisfy", "allocate"):
        for relation in model.relations_of(kind):
            traced.setdefault(relation.source.split("::")[-1], []).append(kind)
    for relation in model.relations_of("dependency"):
        for target in relation.targets:
            traced.setdefault(target.split("::")[-1], []).append("dependency")
    verified = set()
    for relation in model.relations_of("verify"):
        for target in relation.targets:
            verified.add(target.split("::")[-1])

    rows = []
    for requirement in requirements:
        edges = sorted(set(traced.get(requirement.name, [])))
        rows.append(
            [
                c(requirement.short_name),
                c(requirement.name),
                _joined_codes(edges) if edges else "—",
                "yes" if requirement.name in verified else "—",
                maturity_run(requirement),
            ]
        )
    doc.add(
        Table(
            head=["Id", "Requirement", "Traced by", "Verified", "Maturity"],
            rows=rows,
            caption=(
                "A design rule is upheld by a gate rather than allocated to a part, so "
                "the P and N families are expected to be verified but untraced. A goal "
                "requirement is traced by the dependency the stage that discharges it "
                "draws."
            ),
        )
    )


def _ref(model: Model, reference: str):
    element = model.resolve(reference)
    if element is None:
        return [c(reference), t("  (unresolved)")]
    return c(reference)


def section_deferred(model: Model, doc: Doc) -> None:
    deferrals = model.deferred()
    if not deferrals:
        return
    doc.add(Heading(1, "Deferred register"))
    doc.add(
        P(
            "Work a finished stage explicitly left behind, each with the reason that "
            "stage gave. Deferred is not planned: the stage that owns it is closed, and "
            "the item waits for a machine, a workload or a later stage to make it "
            "meaningful."
        )
    )
    doc.add(
        Table(
            head=["Item", "Recorded against", "Stage", "Reason"],
            align=["left", "left", "right", "left"],
            rows=[
                [
                    c(element.name or "—"),
                    c(_owner(element)),
                    stage_text(element),
                    prose(element.deferred_reason),
                ]
                for element in deferrals
            ],
            caption=(
                f"{len(deferrals)} records. The model writes most of them twice — once "
                "against the stage that closed, once against the part that lacks them — "
                "so the register reads from either end."
            ),
        )
    )


def _owner(element: Element) -> str:
    parent = element.parent
    while parent is not None and not parent.name:
        parent = parent.parent
    return parent.qualified_name if parent is not None else "—"


def section_index(model: Model, doc: Doc) -> None:
    doc.add(Heading(1, "Index by stage"))
    doc.add(
        P(
            "Every element carrying @stage, which names the roadmap stage that owns it. "
            "An element with no stage is cross-cutting and does not appear here."
        )
    )
    by_stage = model.by_stage()
    rows = []
    for number, elements in by_stage.items():
        for element in elements:
            rows.append(
                [
                    str(number),
                    c(element.qualified_name),
                    element.kind,
                    maturity_run(element),
                ]
            )
    doc.add(
        Table(
            head=["Stage", "Element", "Kind", "Maturity"],
            align=["right", "left", "left", "left"],
            rows=rows,
            caption=f"{len(rows)} elements across {len(by_stage)} stages.",
        )
    )


def section_figures(model: Model, doc: Doc) -> None:
    """The list of figures, which only has content once the rest has run.

    It sits last for that reason, and reads as back matter: a reader who wants
    the picture of the crate graph and not the prose around it finds it here,
    along with the file it was drawn from.
    """
    placed = doc.meta.get("figures") or []
    if not placed:
        return
    doc.add(Heading(1, "Figures"))
    doc.add(
        P(
            "Every diagram in this document, drawn from the model by "
            "scripts/sysml/diagrams.py. Each is also written as a standalone SVG beside "
            "this file, so it can be opened, zoomed or embedded on its own."
        )
    )
    doc.add(
        Table(
            head=["No.", "Figure", "Shows", "Model file", "File"],
            align=["right", "left", "left", "left", "left"],
            rows=[
                [
                    str(block.number),
                    block.figure.title,
                    f"{len(block.figure.nodes)} nodes, {len(block.figure.edges)} edges",
                    c(block.figure.source) if block.figure.source else "",
                    a(block.file.rsplit("/", 1)[-1], block.file),
                ]
                for block in placed
            ],
            caption=f"{len(placed)} figures.",
        )
    )


# The running order. Append a function here to add to the document.
SECTIONS = (
    section_about,
    section_requirements,
    section_structure,
    section_architectures,
    section_boot,
    section_subsystems,
    section_workspace,
    section_roadmap,
    section_assurance,
    section_traceability,
    section_deferred,
    section_index,
    section_figures,
)


def build(model: Model, banner: str = "") -> Doc:
    doc = Doc(
        title="Ferrix — architecture, from the model",
        subtitle=(
            "Generated from docs/sysml/. Every element carries the maturity keyword "
            "the model gives it."
        ),
        meta={"banner": banner, "digest": model.digest},
    )
    for section in SECTIONS:
        section(model, doc)
    return doc
