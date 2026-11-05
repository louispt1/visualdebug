import json
from pricing import LineItem, Pricer


def test_bulk_discount():
    items = [LineItem("A", 2, 50.0), LineItem("B", 1, 20.0)]
    payload = json.dumps({"n": len(items)})
    assert Pricer("gold", items).total() == 108.0
    assert payload


def test_guest():
    assert Pricer(None, [LineItem("A", -1, 5.0)]).total() == -5.0
