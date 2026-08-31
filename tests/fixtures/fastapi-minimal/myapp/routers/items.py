from fastapi import APIRouter, Depends, HTTPException
from pydantic import BaseModel

from myapp.dependencies import get_db, get_settings

router = APIRouter(prefix="/items")

ITEMS = {1: "hammer"}


class Item(BaseModel):
    id: int
    name: str


@router.get("/{item_id}", response_model=Item)
async def get_item(item_id: int, db=Depends(get_db), settings=Depends(get_settings)):
    try:
        return Item(id=item_id, name=ITEMS[item_id])
    except KeyError:
        raise HTTPException(status_code=404, detail="item not found") from None
