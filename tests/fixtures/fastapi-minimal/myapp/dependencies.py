from contextlib import asynccontextmanager

SETTINGS = {"db_url": "sqlite://"}


def get_settings():
    return SETTINGS


def get_db(settings=None):
    return {"url": (settings or SETTINGS)["db_url"]}


@asynccontextmanager
async def lifespan(app):
    yield
