from decimal import Decimal


class Cart:
    def __init__(self, owner, session_token):
        self.owner = owner
        self.session_token = session_token
        self.items = []

    def add(self, sku, qty, price):
        self.items.append({"sku": sku, "qty": qty, "price": Decimal(price)})
        return self

    def total(self):
        total = Decimal("0")
        for item in self.items:
            total += item["qty"] * item["price"]
        return total


def countdown(n):
    while n > 0:
        yield n
        n -= 1


def parse(text):
    try:
        return int(text)
    except ValueError:
        return None


cart = Cart("ada", session_token="tok-123").add("A", 2, "9.99").add("B", 1, "5.00")
print(cart.total(), list(countdown(3)), parse("x"), parse("4"))
