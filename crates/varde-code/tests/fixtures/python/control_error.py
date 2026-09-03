# Control-flow / error fixtures: ControlFlow, Catch, Throw, plus literals,
# calls, and member accesses to keep each fixture self-describing.


def process(items):
    try:
        if len(items) == 0:
            raise ValueError("empty")
        for item in items:
            if item == 0:
                continue
            while item > 10:
                item = item - 1
                break
    except ValueError as err:
        return str(err)
    finally:
        cleanup()

    with open("data.txt") as fh:
        data = fh.read()

    if data:
        return "ok"
    return None
