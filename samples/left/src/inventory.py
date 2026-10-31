"""Inventory helpers for the warehouse service."""

import json
from dataclasses import dataclass


@dataclass
class Item:
    sku: str
    name: str
    quantity: int
    price: float


def load_items(path):
    # Read the inventory file written by the nightly export.
    with open(path, encoding="utf-8") as f:
        return [Item(**row) for row in json.load(f)]


def total_value(items):
    """Value of everything in stock."""
    return sum(item.quantity * item.price for item in items)


def low_stock(items, threshold=5):
    """Items that need to be reordered."""
    return [item for item in items if item.quantity < threshold]


def format_report(items):
    lines = ["SKU      NAME                 QTY"]
    for item in items:
        lines.append(f"{item.sku:<8} {item.name:<20} {item.quantity:>3}")
    return "\n".join(lines)


def restock(item, amount):
    if amount <= 0:
        raise ValueError("amount must be positive")
    item.quantity += amount
    return item


def main():
    items = load_items("inventory.json")
    print(format_report(items))
    print("Total value:", total_value(items))
    print("Low stock:", [item.sku for item in low_stock(items)])


if __name__ == "__main__":
    main()
