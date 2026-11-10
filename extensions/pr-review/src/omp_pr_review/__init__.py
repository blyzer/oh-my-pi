"""PR review rescue port (ADR 0049 D5).

Pure helpers import without a live omp host. Device registration lives in
``omp_pr_review.device`` and is loaded by the extension entry when the host
imports this package.
"""

from __future__ import annotations

from .findings import (
	AreaBudget,
	Finding,
	finding_schema,
	format_comment,
	parse_area_budgets,
	parse_finding,
	sort_findings,
	validate_ref,
)

__all__ = [
	"AreaBudget",
	"Finding",
	"finding_schema",
	"format_comment",
	"parse_area_budgets",
	"parse_finding",
	"sort_findings",
	"validate_ref",
]
