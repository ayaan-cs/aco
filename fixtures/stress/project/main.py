# Entry point of a small package-based project. The hot loop lives in
# shop/inventory.py, not here.
import helpers
from shop import pricing
from shop.inventory import restock_report

items = helpers.make_items(6000)
print(pricing.total(items[:50]))
print(restock_report(items))
