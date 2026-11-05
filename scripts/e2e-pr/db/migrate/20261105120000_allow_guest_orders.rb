class AllowGuestOrders < ActiveRecord::Migration[8.1]
  def change
    change_column_null :orders, :customer_id, true
  end
end
