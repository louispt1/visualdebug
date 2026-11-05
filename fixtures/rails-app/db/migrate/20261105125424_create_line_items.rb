class CreateLineItems < ActiveRecord::Migration[8.1]
  def change
    create_table :line_items do |t|
      t.references :order, null: false, foreign_key: true
      t.string :sku
      t.integer :quantity
      t.decimal :unit_price

      t.timestamps
    end
  end
end
