"""Dump the parsed model as JSON, so nothing else has to parse SysML.

This is the surface another tool -- or another agent -- should build on. It is
the whole model, not a summary: every element with its qualified name, kind,
maturity, stage, doc comment and children, plus every relation. A checker that
wants to assert "no `#implemented` element sits in a stage that is still
`Planned`" reads this file and never learns the notation.

The figures the document draws are here too, as the graphs they are: nodes and
edges with the qualified names they came from. A tool that wants to draw the
model its own way -- with Graphviz, in a browser, into a slide -- takes those
and skips both the notation and the layout.

The shape is stable in one direction: fields are added, never renamed or
removed, and `schema` carries the version that promise is made against.
"""

from __future__ import annotations

import json

from .model import Element, Model

SCHEMA = 1


def element_to_dict(element: Element) -> dict:
    """One element, with empty fields omitted so the file stays readable."""
    out: dict = {
        "name": element.name,
        "qualifiedName": element.qualified_name,
        "kind": element.kind,
    }
    if element.short_name:
        out["shortName"] = element.short_name
    if element.is_definition:
        out["definition"] = True
    if element.modifiers:
        out["modifiers"] = element.modifiers
    if element.typed_by:
        out["type"] = element.typed_by
    if element.specializes:
        out["specializes"] = element.specializes
    if element.redefines:
        out["redefines"] = element.redefines
    if element.multiplicity:
        out["multiplicity"] = element.multiplicity
    if element.value:
        out["value"] = element.value
    if element.maturity:
        out["maturity"] = element.maturity
    if element.stage is not None:
        out["stage"] = element.stage
    if element.deferred_reason:
        out["deferred"] = element.deferred_reason
    if element.doc:
        out["doc"] = element.doc
    if element.flow:
        out["flow"] = element.flow
    if element.successions:
        out["successions"] = [
            {"from": source, "to": target, "guard": guard}
            for source, target, guard in element.successions
        ]
    out["source"] = {"file": element.source, "line": element.line}
    if element.children:
        out["children"] = [element_to_dict(child) for child in element.children]
    return out


def figure_to_dict(figure) -> dict:
    """One figure as a graph: what it is of, and what it holds."""
    out = {
        "name": figure.name,
        "title": figure.title,
        "kind": figure.kind,
        "direction": figure.direction,
        "caption": figure.caption,
        "nodes": [],
        "edges": [
            {"from": edge.source, "to": edge.target, "kind": edge.kind, "label": edge.label}
            for edge in figure.edges
        ],
    }
    for node in figure.nodes:
        entry = {"id": node.id, "label": node.label, "kind": node.kind}
        if node.sublabel:
            entry["sublabel"] = node.sublabel
        if node.maturity:
            entry["maturity"] = node.maturity
        if node.stage is not None:
            entry["stage"] = node.stage
        if node.cluster:
            entry["cluster"] = node.cluster
        out["nodes"].append(entry)
    if figure.clusters:
        out["clusters"] = [
            {"id": cluster.id, "label": cluster.label} for cluster in figure.clusters
        ]
    return out


def model_to_dict(model: Model, figures: list | None = None) -> dict:
    counts = model.maturity_counts()
    return {
        "schema": SCHEMA,
        "digest": model.digest,
        "files": [
            {"path": path, "synopsis": model.synopses.get(path, "")} for path in model.files
        ],
        "summary": {
            "packages": len(model.packages()),
            "elements": sum(1 for _ in model.walk()),
            "maturity": counts,
            "deferred": len(model.deferred()),
            "relations": len(model.relations),
            "figures": len(figures or []),
            "unparsed": len(model.unparsed),
        },
        "packages": [element_to_dict(root) for root in model.roots],
        "relations": [
            {
                "kind": relation.kind,
                "source": relation.source,
                "targets": relation.targets,
                "origin": relation.origin,
                "file": relation.file,
                "line": relation.line,
            }
            for relation in model.relations
        ],
        "figures": [figure_to_dict(figure) for figure in (figures or [])],
        "unparsed": [
            {"file": element.source, "line": element.line, "text": element.raw}
            for element in model.unparsed
        ],
    }


def render(model: Model, figures: list | None = None) -> str:
    """Pretty JSON with sorted keys nowhere: insertion order is source order."""
    return json.dumps(model_to_dict(model, figures), indent=2, ensure_ascii=False) + "\n"
