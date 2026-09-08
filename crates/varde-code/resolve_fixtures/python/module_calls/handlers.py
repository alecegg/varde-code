from svc import users
from svc import items

def handle(x):
    users.create(x)
    items.create(x)
