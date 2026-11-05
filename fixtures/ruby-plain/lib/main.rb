require_relative "inventory"

inv = Inventory.new([Inventory::Item.new("A", 1, 2.5), Inventory::Item.new("B", 5, 1.0), Inventory::Item.new("C", 0, 4.0)])
config = { region: "eu", password: "hunter2" }
added = inv.restock(3)
puts "added #{added}, value #{inv.value}, qty #{inv.parse_qty('7 items')}, depth #{inv.depth(3)}, #{config[:region]}"
