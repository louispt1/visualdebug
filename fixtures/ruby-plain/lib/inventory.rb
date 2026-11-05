class Inventory
  Item = Struct.new(:sku, :qty, :price)

  def initialize(items, api_token: "s3cret")
    @items = items
    @api_token = api_token
  end

  def restock(min)
    added = 0
    i = 0
    while i < @items.size
      item = @items[i]
      if item.qty < min
        added += min - item.qty
        item.qty = min
      end
      i += 1
    end
    added
  end

  def value
    total = 0
    @items.each { |item| total += item.qty * item.price }
    total
  end

  def parse_qty(text)
    attempts = 0
    begin
      attempts += 1
      Integer(text)
    rescue ArgumentError
      text = text.delete("^0-9")
      retry if attempts < 2
      0
    end
  end

  def depth(n)
    n.zero? ? 0 : 1 + depth(n - 1)
  end
end
