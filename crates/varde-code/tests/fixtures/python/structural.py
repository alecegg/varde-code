# Structural fixtures: Function, Class, Parameter, MemberAccess, Variable.
# Note: Python has no interface construct (ABCs are runtime classes) — the
# Interface kind is carved out for python (see REQUIRED_KINDS in
# src/extract/langs/python.rs).

def greet(name: str) -> str:
    return f"Hello {name}"


class Greeter:
    greeting: str = ""

    def __init__(self, greeting: str = "hi"):
        self.greeting = greeting

    def greet(self, other: str = "world") -> str:
        return f"Hello {other}"


def compute(a, b=2, *args, c: int, **kwargs):
    total = a + b
    return total
