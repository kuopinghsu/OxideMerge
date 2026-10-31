"""Inventory helpers for the warehouse service."""

import json
import logging
from dataclasses import dataclass

log = logging.getLogger(__name__)


@dataclass
class Item:
    sku: str
    name: str
    quantity: int
    price: float
    location: str = "MAIN"


def load_items(path):
    # Read the inventory file that the nightly export writes.
    with open(path, encoding="utf-8") as f:
        return [Item(**row) for row in json.load(f)]


def restock(item, amount):
    if amount <= 0:
        raise ValueError("amount must be positive")
    item.quantity += amount
    log.info("restocked %s by %d", item.sku, amount)
    return item


def total_value(items):
    """Value of everything in stock."""
    return sum(item.quantity * item.price  for item in items)


def low_stock(items, threshold=10):
    """Items that need to be reordered."""
    return [item for item in items if item.quantity < threshold]


def format_report(items):
    lines = ["SKU      NAME                 QTY  LOCATION"]
    for item in items:
        lines.append(
            f"{item.sku:<8} {item.name:<20} {item.quantity:>3}  {item.location}"
        )
    return "\n".join(lines)



def main():
    items = load_items("inventory.json")
    print(format_report(items))
    print("Total value:", round(total_value(items), 2))
    print("Low stock:", [item.sku for item in low_stock(items)])


if __name__ == "__main__":
    main()
