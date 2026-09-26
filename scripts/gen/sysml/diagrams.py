"""Turn the model into figures. The only module that decides what to draw.

`sections.py` is the extension point for the document; this is the extension
point for its diagrams, and the two keep the same rule: **say what the model
says**. Every node here is an element the model declares, every edge is a
relation it writes down, and the colour of a box is the lifecycle keyword the
element carries. Nothing is arranged for effect, and nothing is invented to
fill a gap -- a figure with no edges is dropped rather than padded, because a
diagram of one box is a sentence with a border around it.

What is drawn is chosen by *shape*, not by name, so the figures follow the
model as it grows:

    every action whose body has a succession        -> a flow diagram
    every state definition with a transition        -> a state machine
    every definition with three or more subtypes    -> a generalisation
    every part with connected ports                 -> an interface diagram
    every group of `dependency` statements          -> a dependency graph
    `satisfy` / `allocate` / `verify`               -> traceability

A stage added to the roadmap, a crate added to the workspace or a subtype added
to `KernelObject` appears in its figure with no change here. The thresholds --
three subtypes, two steps -- exist so that a diagram is worth the reader's
attention, and they are the only judgement in the file.
"""

from __future__ import annotations

from .figure import Cluster, Figure, Node, slugify
from .model import MATURITY_LABEL, Element, Model, humanise

MIN_SUBTYPES = 3
"""Below this a generalisation is a sentence: "X and Y specialize Z"."""

MIN_STEPS = 3
"""A flow of two steps is an arrow, and the numbered list above it is better."""


# ---------------------------------------------------------------------------
# Nodes from elements
# ---------------------------------------------------------------------------


def _node(
    element: Element,
    *,
    kind: str = "box",
    label: str = "",
    sublabel: str = "",
    node_id: str = "",
    rows: list[str] | None = None,
) -> Node:
    return Node(
        id=node_id or element.qualified_name,
        label=label or element.name or element.typed_by,
        sublabel=sublabel,
        kind=kind,
        maturity="deferred" if element.is_deferred else element.maturity,
        stage=element.stage,
        rows=rows or [],
        doc=element.doc,
    )


def _stage_note(element: Element) -> str:
    return f"stage {element.stage}" if element.stage is not None else ""


def _type_note(element: Element) -> str:
    return f": {element.typed_by}" if element.typed_by else ""


def _source_of(element: Element) -> str:
    return element.source.rsplit("/", 1)[-1] if element.source else ""


# ---------------------------------------------------------------------------
# Flows: an action body as a diagram
# ---------------------------------------------------------------------------


def flow(model: Model, qualified: str, *, direction: str = "TB") -> Figure | None:
    """One action's successions, branches included."""
    action = model.find(qualified)
    if action is None or len(action.successions) < MIN_STEPS:
        return None

    figure = Figure(
        name=slugify(qualified),
        title=humanise(action.name),
        kind="flow",
        direction=direction,
        source=_source_of(action),
    )
    for source, target, guard in action.successions:
        for step in (source, target):
            figure.add(_step_node(action, step))
        figure.link(_step_id(action, source), _step_id(action, target), "flow", guard)

    figure.caption = _flow_caption(action)
    return None if figure.is_empty else figure


def _step_id(action: Element, step: str) -> str:
    return f"{action.qualified_name}::{step}"


def _step_node(action: Element, step: str) -> Node:
    """A step of a flow: the child action if there is one, else a marker.

    `start`, `done` and `decide` are the notation's own words rather than
    elements, so they become the shapes that mean them.
    """
    if step in ("start", "done"):
        return Node(id=_step_id(action, step), label=step, kind="terminal")
    if step == "decide":
        return Node(id=_step_id(action, step), label="decide", kind="choice")
    child = action.child(step)
    if child is None:
        return Node(id=_step_id(action, step), label=step, kind="action")
    return _node(
        child,
        kind="action",
        node_id=_step_id(action, step),
        sublabel=_stage_note(child),
    )


def _flow_caption(action: Element) -> str:
    steps = len({name for pair in action.successions for name in pair[:2]})
    branches = sum(1 for succession in action.successions if succession[2])
    caption = f"{steps} steps, as `{action.name}` orders them"
    if branches:
        caption += f", with {branches} guarded branch" + ("es" if branches > 1 else "")
    return caption + "."


def flows(model: Model, package_name: str) -> list[Figure]:
    """Every action in a package whose body has enough of a body to draw."""
    package = model.package(package_name)
    if package is None:
        return []
    out: list[Figure] = []
    for element in package.walk():
        if element.kind != "action" or len(element.successions) < MIN_STEPS:
            continue
        figure = flow(model, element.qualified_name)
        if figure is not None:
            out.append(figure)
    return out


# ---------------------------------------------------------------------------
# State machines
# ---------------------------------------------------------------------------


def state_machines(model: Model, package_name: str = "") -> list[Figure]:
    out: list[Figure] = []
    for element in model.walk():
        transitions = [child for child in element.children if child.kind == "transition"]
        if not transitions:
            continue
        if package_name and element.package != package_name:
            continue
        figure = Figure(
            name=slugify(element.qualified_name),
            title=humanise(element.name),
            kind="state",
            direction="TB",
            source=_source_of(element),
        )
        for child in element.children:
            if child.kind == "state":
                figure.add(_node(child, kind="state", sublabel=_stage_note(child)))
        entry = element.flow[0] if element.flow else ""
        if entry and element.child(entry) is not None:
            start = Node(id=f"{element.qualified_name}::start", label="start", kind="terminal")
            figure.add(start)
            figure.link(start.id, f"{element.qualified_name}::{entry}", "transition")
        for transition in transitions:
            if len(transition.flow) != 2:
                continue
            source, target = transition.flow
            figure.link(
                f"{element.qualified_name}::{source}",
                f"{element.qualified_name}::{target}",
                "transition",
                transition.value.split(":")[0].strip() if transition.value else "",
            )
        figure.caption = (
            f"{len(figure.nodes) - 1} states and {len(transitions)} transitions; a label "
            "is the event the transition accepts."
        )
        if not figure.is_empty:
            out.append(figure)
    return out


# ---------------------------------------------------------------------------
# Structure: composition, generalisation, connection
# ---------------------------------------------------------------------------


MIN_MEMBERS = 4
"""Below this the table of features above the figure already says it."""


def composition(
    model: Model,
    qualified: str,
    *,
    direction: str = "LR",
    kinds: tuple[str, ...] = ("part",),
    minimum: int = MIN_MEMBERS,
) -> Figure | None:
    """A part and what it is made of, one level down.

    One level, not two: the members' own members are their own figure, and a
    tree three deep drawn on one page is a page nobody reads.
    """
    owner = model.find(qualified)
    if owner is None:
        return None

    figure = Figure(
        name=slugify(qualified),
        title=f"{humanise(owner.name)} and its parts",
        kind="block",
        direction=direction,
        source=_source_of(owner),
    )
    figure.add(_node(owner, kind="def", sublabel=_stage_note(owner)))
    for child in owner.children:
        if child.kind not in kinds or not child.name:
            continue
        member = figure.add(
            _node(
                child,
                sublabel=_type_note(child)
                + (f"  {child.multiplicity}" if child.multiplicity else ""),
            )
        )
        figure.link(owner.qualified_name, member.id, "composition")
    if len(figure.edges) < minimum:
        return None
    figure.caption = (
        f"The parts `{owner.name}` is made of, coloured by the lifecycle keyword each carries."
    )
    return figure


def generalisations(model: Model, package_name: str = "") -> list[Figure]:
    """Every definition with enough subtypes to be worth a picture.

    The model writes `:>` in six places and means the same thing each time: a
    kernel object, a filesystem, an architecture. Finding them by shape rather
    than by name means the seventh appears here on its own.
    """
    families: dict[str, list[Element]] = {}
    for element in model.walk():
        if not element.specializes:
            continue
        if package_name and element.package != package_name:
            continue
        for base in element.specializes.split(","):
            families.setdefault(base.strip(), []).append(element)

    out: list[Figure] = []
    for base_name, subtypes in families.items():
        if len(subtypes) < MIN_SUBTYPES:
            continue
        base = model.resolve(base_name)
        if base is None:
            continue
        figure = Figure(
            name=slugify(base.qualified_name),
            title=f"{humanise(base.name)} and its subtypes",
            kind="block",
            direction="TB",
            flip=True,
            source=_source_of(base),
        )
        figure.add(
            _node(
                base,
                kind="def",
                sublabel=_stage_note(base),
                rows=[
                    f"{child.kind} {child.name}"
                    for child in base.children
                    if child.kind in ("action", "attribute", "part", "port") and child.name
                ][:6],
            )
        )
        for subtype in subtypes:
            node = figure.add(_node(subtype, sublabel=_stage_note(subtype)))
            figure.link(node.id, base.qualified_name, "specialization")
        figure.caption = (
            f"{len(subtypes)} definitions specialize `{base.name}`; the hollow arrow points "
            "at what they have in common."
        )
        if not figure.is_empty:
            out.append(figure)
    return out


def connections(model: Model, *qualified: str) -> Figure | None:
    """The parts that meet at a port, and the ports they meet at.

    An interface diagram, built from `connect` -- the one statement in the
    model that names a wire rather than a thing. Each part carries its ports as
    a compartment, so the box says what it offers and the line says who took it.
    """
    owners = [model.find(name) for name in qualified]
    owners = [owner for owner in owners if owner is not None]
    if not owners:
        return None

    figure = Figure(
        name="interfaces",
        title="The pieces and the ports between them",
        kind="interface",
        direction="LR",
        source=_source_of(owners[0]),
    )
    drawn_types = {owner.name for owner in owners}
    members: dict[str, Element] = {}
    for owner in owners:
        cluster_id = owner.qualified_name
        has_members = False
        for child in owner.children:
            if child.kind != "part" or not child.name:
                continue
            if child.typed_by in drawn_types:
                # `part ferrix : Ferrix` inside the deployment, where Ferrix's
                # own members are on the diagram: drawing both is drawing the
                # same thing twice, and the box would sit inside its own
                # contents. The connections into it land on those members.
                members[child.name] = child
                continue
            target = model.resolve(child.typed_by) if child.typed_by else None
            ports = [
                f"{port.name} : {port.typed_by.lstrip('~')}"
                for port in (target.children if target is not None else [])
                if port.kind == "port" and port.name
            ]
            node = figure.add(
                _node(child, sublabel=_type_note(child), rows=ports[:4])
            )
            node.cluster = cluster_id
            members[child.name] = child
            has_members = True
        if has_members and len(owners) > 1:
            figure.clusters.append(Cluster(id=cluster_id, label=owner.name))

    for relation in model.relations_of("connect"):
        source = _chain_node(model, figure, members, relation.source, relation.origin)
        for target_chain in relation.targets:
            target = _chain_node(model, figure, members, target_chain, relation.origin)
            if source is None or target is None:
                continue
            label = f"{relation.source.split('.')[-1]} → {target_chain.split('.')[-1]}"
            figure.link(source, target, "connect", label)

    figure.caption = (
        "Each box lists the ports it declares; each line is a `connect` statement, "
        "labelled with the two ports it joins."
    )
    return None if figure.is_empty else figure


def _chain_node(
    model: Model, figure: Figure, members: dict[str, Element], chain: str, scope: str = ""
) -> str | None:
    """Find the box a feature chain such as `ferrix.loader.efi` lands on.

    The chain walks members: `ferrix` is a part, `loader` a part of its type,
    `efi` a port on that. The box is the deepest segment that is drawn, which
    is how a connection written at the deployment level still lands on the
    loader rather than on the whole system.
    """
    found: str | None = None
    for segment in chain.split("."):
        element = members.get(segment)
        if element is not None and figure.node(element.qualified_name) is not None:
            found = element.qualified_name
            continue
        resolved = model.resolve(segment, scope)
        if resolved is not None and figure.node(resolved.qualified_name) is not None:
            found = resolved.qualified_name
    return found


# ---------------------------------------------------------------------------
# Dependency graphs
# ---------------------------------------------------------------------------


def dependencies(
    model: Model,
    scope: str,
    *,
    title: str,
    name: str,
    direction: str = "LR",
    sublabel_attribute: str = "",
    include_members: bool = False,
) -> Figure | None:
    """Every `dependency` written inside one element, as a graph.

    The workspace states its crate graph this way, and Cargo.toml states it
    again; a picture of the first is a picture of the intent, which is the one
    a reader can argue with.
    """
    owner = model.find(scope)
    if owner is None:
        return None

    figure = Figure(
        name=name,
        title=title,
        kind="dependency",
        direction=direction,
        source=_source_of(owner),
    )
    relations = [
        relation
        for relation in model.relations_of("dependency")
        if relation.origin == scope or relation.origin.startswith(scope + "::")
    ]
    if include_members:
        # Every member, not only the ones an edge touches. A crate nothing
        # depends on is the point of the picture: it is written ahead, and the
        # empty space around it is what "nothing calls it yet" looks like.
        for child in owner.children:
            if child.kind != "part" or child.is_definition or not child.name:
                continue
            sublabel = ""
            if sublabel_attribute:
                sublabel = child.attribute_value(sublabel_attribute).strip('" ')
            figure.add(_node(child, sublabel=sublabel or _stage_note(child)))
    for relation in relations:
        for reference in [relation.source, *relation.targets]:
            element = model.resolve(reference, relation.origin)
            if element is None:
                continue
            sublabel = ""
            if sublabel_attribute:
                sublabel = element.attribute_value(sublabel_attribute).strip('" ')
            figure.add(_node(element, sublabel=sublabel or _stage_note(element)))
    for relation in relations:
        source = model.resolve(relation.source, relation.origin)
        if source is None:
            continue
        for reference in relation.targets:
            target = model.resolve(reference, relation.origin)
            if target is None:
                continue
            figure.link(source.qualified_name, target.qualified_name, "dependency")

    figure.caption = (
        f"{len(figure.edges)} `dependency` statements; an arrow points from the thing that "
        "needs to the thing it needs."
    )
    return None if figure.is_empty else figure


def stages(model: Model) -> Figure | None:
    """The roadmap as the graph its `dependency` statements already make.

    Read down: nothing starts until what it points at is done. The colour is
    the lifecycle keyword, so the boundary between what runs and what is owed
    is the boundary between the greens and the greys.
    """
    package = model.package("FerrixRoadmap")
    if package is None:
        return None
    figure = Figure(
        name="roadmap-stages",
        title="The roadmap, stage by stage",
        kind="dependency",
        direction="TB",
        source=_source_of(package),
    )
    for element in package.children:
        if element.kind != "requirement" or element.is_definition or not element.name:
            continue
        # A stage is what the model types as one. The package also holds
        # design rules the roadmap defers, and those belong to another figure.
        if not element.typed_by.split("::")[-1] == "Stage":
            continue
        size = element.attribute_value("size").strip('" ')
        status = element.attribute_value("status").split("::")[-1]
        note = " · ".join(part for part in (status, size) if part)
        figure.add(
            _node(
                element,
                kind="requirement",
                label=(f"{element.short_name}  " if element.short_name else "")
                + humanise(element.name),
                sublabel=note,
            )
        )
    for relation in model.relations_of("dependency"):
        source = model.resolve(relation.source, relation.origin)
        if source is None or figure.node(source.qualified_name) is None:
            continue
        for reference in relation.targets:
            target = model.resolve(reference, relation.origin)
            if target is None:
                continue
            # A stage that depends on a goal is a different figure; here the
            # edges wanted are the ones between stages.
            figure.link(target.qualified_name, source.qualified_name, "dependency")
    figure.caption = (
        "An arrow points from a stage to the stage it unblocks. The two stages with a "
        "second arrow into them are the ones that need more than their predecessor."
    )
    return None if figure.is_empty else figure


# ---------------------------------------------------------------------------
# Traceability
# ---------------------------------------------------------------------------


def trace(
    model: Model,
    kinds: tuple[str, ...],
    *,
    name: str,
    title: str,
    caption: str,
    origin_prefix: str = "",
    source_kind: str = "requirement",
    target_kind: str = "box",
    direction: str = "LR",
) -> Figure | None:
    """One or more relation kinds drawn as the bipartite graph they are.

    `satisfy`, `allocate` and `verify` all say the same shape of thing -- this
    one is answered by that one -- and the value of drawing them is seeing the
    node with no line into it.
    """
    figure = Figure(name=name, title=title, kind="trace", direction=direction)
    for kind in kinds:
        for relation in model.relations_of(kind):
            if origin_prefix and not relation.origin.startswith(origin_prefix):
                continue
            source = model.resolve(relation.source, relation.origin)
            if source is None:
                continue
            figure.source = figure.source or _source_of(source)
            figure.add(
                _node(
                    source,
                    kind=source_kind,
                    label=(f"{source.short_name}  " if source.short_name else "")
                    + humanise(source.name),
                    sublabel=_stage_note(source),
                )
            )
            for reference in relation.targets:
                target = model.resolve(reference, relation.origin)
                if target is None:
                    continue
                name = target.name or reference
                figure.add(
                    _node(
                        target,
                        kind=target_kind,
                        label=(f"{target.short_name}  " if target.short_name else "")
                        + (humanise(name) if target_kind == "requirement" else name),
                        sublabel=_chain_label(reference),
                    )
                )
                figure.link(
                    source.qualified_name,
                    target.qualified_name,
                    kind,
                    # With one kind on the diagram the caption can say which it
                    # is; with two, only the line can.
                    kind if len(kinds) > 1 else "",
                )
    figure.caption = caption
    return None if figure.is_empty else figure


def _chain_label(reference: str) -> str:
    """`ferrix.kernel.mm` under a box called `mm`, so the path is not lost.

    A reference that is already just a name repeats the label, and a box that
    says the same thing twice is a box with less room for the thing it says.
    """
    return reference if "." in reference else ""


def goals(model: Model) -> Figure | None:
    """The goal, its parts, and the stage each part waits for."""
    package = model.package("FerrixRequirements")
    if package is None:
        return None
    goal = None
    for element in package.children:
        if element.kind == "requirement" and not element.is_definition and element.children:
            goal = element
            break
    if goal is None:
        return None

    figure = Figure(
        name="goal-decomposition",
        title=humanise(goal.name),
        kind="trace",
        direction="LR",
        source=_source_of(goal),
    )
    figure.add(
        _node(
            goal,
            kind="requirement",
            label=(f"{goal.short_name}  " if goal.short_name else "") + humanise(goal.name),
        )
    )
    for child in goal.children:
        if child.kind != "requirement" or not child.name:
            continue
        node = figure.add(
            _node(
                child,
                kind="requirement",
                label=(f"{child.short_name}  " if child.short_name else "")
                + humanise(child.name),
            )
        )
        figure.link(goal.qualified_name, node.id, "composition")

    for relation in model.relations_of("dependency"):
        source = model.resolve(relation.source, relation.origin)
        if source is None:
            continue
        for reference in relation.targets:
            target = model.resolve(reference, relation.origin)
            if target is None or figure.node(target.qualified_name) is None:
                continue
            if source.package != "FerrixRoadmap":
                continue
            figure.add(
                _node(
                    source,
                    kind="requirement",
                    label=(f"{source.short_name}  " if source.short_name else "")
                    + humanise(source.name),
                    sublabel=source.attribute_value("status").split("::")[-1],
                )
            )
            figure.link(source.qualified_name, target.qualified_name, "dependency")

    figure.caption = (
        "The goal's parts, and the roadmap stage each one waits for. A part with no stage "
        "pointing at it is one nothing on the roadmap has claimed yet."
    )
    return None if figure.is_empty else figure


# ---------------------------------------------------------------------------
# The legend the maturity colours need, as prose the document can print
# ---------------------------------------------------------------------------


def maturity_legend(figure: Figure) -> list[tuple[str, str]]:
    return [(maturity, MATURITY_LABEL.get(maturity, maturity)) for maturity in figure.maturities()]
