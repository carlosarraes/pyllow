from fastapi import APIRouter

from myapp.services import format_user

router = APIRouter(prefix="/users")


@router.get("/")
async def list_users():
    return [format_user(1)]


@router.get("/{user_id}")
async def get_user(user_id: int):
    return format_user(user_id)
