from app import crud

def login(u, p):
    if crud.authenticate(u, p):
        return crud.get_user(u)
    return None
