"""Read `docs/sysml/` and generate documents from it.

The pipeline has four stages, and each is a separate module so that adding to
the document never means touching the parser:

    parser.py    SysML v2 text            -> model.Model
    sections.py  model.Model              -> document.Doc
    render_*.py  document.Doc             -> Markdown / HTML
    emit_json.py model.Model              -> JSON

`sections.py` is the extension point. A new part of the document is a new
function appended to its `SECTIONS` list; it receives the model and returns
document blocks, and all three outputs gain it at once. Nothing else has to
change, and no renderer knows what a requirement is.

`gen-arch-doc.py` at the top of `scripts/` is the command that runs all four.
"""

from __future__ import annotations

from .model import MATURITIES, MATURITY_LABEL, Element, Model, Relation
from .parser import load, parse_text

__all__ = [
    "MATURITIES",
    "MATURITY_LABEL",
    "Element",
    "Model",
    "Relation",
    "load",
    "parse_text",
]
