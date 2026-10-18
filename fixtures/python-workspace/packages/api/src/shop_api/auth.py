"""API tokens and scopes."""

SCOPES = frozenset({"checkout", "admin"})


def require_scope(token: str, scope: str) -> None:
    if scope not in SCOPES or not token.endswith(":" + scope):
        raise PermissionError(f"token lacks scope {scope!r}")
