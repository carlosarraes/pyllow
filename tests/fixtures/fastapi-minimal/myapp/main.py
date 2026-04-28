from fastapi import FastAPI

from myapp.routers import users

app = FastAPI(title="demo")
app.include_router(users.router)
