# Imports the re-exported name from the package root, NOT the submodule.
from pkg import Thing


def use_it():
    return Thing().run()
