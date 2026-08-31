from fastapi import FastAPI

from myapp.dependencies import lifespan
from myapp.routers import items, users

app = FastAPI(title="demo", lifespan=lifespan)
app.include_router(users.router)
app.include_router(items.router)
