class OrdersController < ApplicationController
  def price
    order = Order.find(params[:id])
    total = Pricer.new(order).call
    render json: { id: order.id, total: total.to_f }
  end
end
