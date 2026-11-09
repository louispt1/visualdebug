# visualdebug: Ruby / Rails

Injected with `RUBYOPT`; no Gemfile changes. Ruby 3.2+ (best on 3.3+). Spring is disabled and Rails
parallel tests forced to one worker automatically.

## Commands
```
# one RSpec example, starting where the change's behaviour starts
visualdebug run --focus-diff main --start-at "Pricer#call" -- bundle exec rspec spec/services/pricer_spec.rb:42
# a request spec, starting at the controller action
visualdebug run --focus-diff main --start-at "OrdersController#create" -- bundle exec rspec spec/requests/orders_spec.rb:10
# Minitest / Rails tests (one test by line)
visualdebug run --focus-diff main -- bin/rails test test/models/order_test.rb:15
# a script or runner
visualdebug run --focus-diff main -- bundle exec rails runner 'Pricer.new(Order.first).call'
visualdebug run -- ruby lib/main.rb
```

## `--start-at` forms
- `Pricer#call` (instance method), `Pricer.build` (class method), `Admin::Pricer#call`
- `app/services/pricer.rb:18`: the method containing that line
- Without `--start-at`, RSpec and Minitest record **each test body** (setup/`before` blocks excluded,
  but their effects are visible as state). Plain scripts record the whole program.
- A start target re-arms: every call to it opens a new window in the same trace.

## Reading Ruby traces
- Locals include the receiver's instance variables as `@name`.
- Library calls (gems, Rails) appear once as `→ ActiveRecord::Persistence#update! (library)` with the
  return value; blocks you pass to library code (e.g. `each`) are traced as `block in Pricer#call`.
- SQL from ActiveRecord appears as `SQL` side-effect rows attached to the line that triggered it
  (schema introspection queries are hidden). Lazy loading shows up as a `SELECT` at the line that
  first touches the association, often worth a tour stop.
- `ActiveRecord::Relation`s and associations are shown as `unloaded` until something loads them;
  the probe never loads them itself. Models show their attributes and `(changed=...)`.
- `BigDecimal` is shown as `90.0`.

## Examples you write
Put temporary specs in `.visualdebug/examples/`, e.g. `.visualdebug/examples/guest_discount_spec.rb`
with `require "rails_helper"` (RSpec resolves it via the project's `spec/` load path), then run
`bundle exec rspec .visualdebug/examples/guest_discount_spec.rb`. Use factories/fixtures the project
already has; transactional tests keep the database clean. Never modify app code.

## Gotchas
- Huge apps: always `--start-at`, since Rails boot and test setup otherwise fill the trace.
- `bundle exec` must work in the project first (`visualdebug doctor` checks).
- Code run in other threads (e.g. ActionCable, concurrent-ruby pools) is only recorded in
  whole-program mode.
