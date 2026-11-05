require "minitest/autorun"
require_relative "../lib/inventory"

class InventoryTest < Minitest::Test
  def setup
    @inv = Inventory.new([Inventory::Item.new("A", 1, 2.0)])
  end

  def test_restock_tops_up_low_items
    added = @inv.restock(3)
    assert_equal 2, added
  end
end
