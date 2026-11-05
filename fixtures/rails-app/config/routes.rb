Rails.application.routes.draw do
  post "orders/:id/price", to: "orders#price"
end
