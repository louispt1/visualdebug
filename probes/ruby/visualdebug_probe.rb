# frozen_string_literal: true

# visualdebug Ruby probe. Injected with RUBYOPT=-r<this file>; configured by the JSON file at
# $VISUALDEBUG_CONFIG. Zero dependencies. Every hook swallows its own errors (logged to
# <out_dir>/probe-errors.log): a probe bug must never change the traced program's behaviour.
require "json"

module VisualdebugProbe
  VERSION = 1
  SELF_PATH = File.expand_path(__FILE__)

  class << self
    attr_reader :cfg

    # ---------------------------------------------------------------- setup
    def install(cfg)
      @cfg = cfg
      @root = cfg.fetch("root").chomp("/") + "/"
      @out = cfg.fetch("out_dir")
      @line_files = cfg["line_files"]&.to_h { |f| [f, true] } # nil => every project file
      @max_events = cfg.fetch("max_events", 20_000)
      @redact = Regexp.new(cfg["redact"] || "password|passwd|secret|token|api_key|apikey|authorization|cookie|session|private_key|credential", Regexp::IGNORECASE)
      limits = cfg["limits"] || {}
      @max_repr = limits.fetch("repr", 200)
      @max_items = limits.fetch("items", 10)
      @max_depth = limits.fetch("depth", 3)
      @seq = 0
      @count = 0
      @buf = []
      @io = nil
      @io_pid = nil
      @frame_seq = 0
      @truncated = false
      @path_cache = {}
      @armed = {} # "Const#meth" => handler
      @redacted_ids = {} # object ids of values redacted by name, so they stay redacted as return values etc.
      @main_script = File.expand_path($PROGRAM_NAME) rescue nil
      Dir.mkdir(@out) unless Dir.exist?(@out)

      @main = TracePoint.new(:call, :return, :b_call, :b_return, :line, :raise) { |tp| safely("event") { on_event(tp) } }
      emit_meta(kind: "start", language: "ruby", runtime: RUBY_VERSION, probe_version: VERSION, argv: [$PROGRAM_NAME, *ARGV])

      start = cfg["start_at"]
      case start
      when nil then @main.enable # whole program
      when "auto:test" then arm_tests
      when /\A(.+):(\d+)\z/ then arm_file_line(File.expand_path(Regexp.last_match(1), @root), Regexp.last_match(2).to_i)
      when /\A(.+?)(#|\.)([^#.]+)\z/ then arm(Regexp.last_match(1), Regexp.last_match(3).to_sym, Regexp.last_match(2) == ".") { |tp| start_window(tp) }
      else log_error("install", ArgumentError.new("unrecognised start_at #{start.inspect}"))
      end
      at_exit { finish }
    rescue StandardError, ScriptError => e
      log_error("install", e)
    end

    def finish
      @main&.disable
      flush
    rescue StandardError
      nil
    end

    # Every TracePoint hook body goes through this: an exception escaping a hook would surface in
    # the traced program.
    def safely(where)
      yield
    rescue StandardError, ScriptError, SystemStackError => e
      log_error(where, e)
      nil
    end

    # ---------------------------------------------------------------- arming
    # Arm a handler on Const#meth (or Const.meth). Costs nothing per call: a :end TracePoint (one
    # event per class/module body) waits for the method to exist, then a TracePoint *targeted* at
    # that one method fires the handler. :line is included because a TracePoint enabled inside a
    # :call hook misses the method's first line (same instruction); first_line re-emits it.
    def arm(const_name, meth, singleton, &handler)
      key = "#{const_name}#{singleton ? '.' : '#'}#{meth}"
      @armed[key] = [const_name, meth, singleton, handler]
      try_arm(key) || ensure_end_watcher
    end

    def ensure_end_watcher
      @end_watcher ||= TracePoint.new(:end) do |tp|
        @armed.each_key { |k| try_arm(k) } if @armed.any? { |_, (c, *)| c == (tp.self.name rescue nil) }
      rescue StandardError, ScriptError => e
        log_error("end_watcher", e)
      end
      @end_watcher.enable unless @end_watcher.enabled?
    end

    def try_arm(key)
      const_name, meth, singleton, handler = @armed[key]
      return false unless handler
      mod = resolve_const(const_name) or return false
      owner = singleton ? mod.singleton_class : mod
      return false unless owner.method_defined?(meth) || owner.private_method_defined?(meth)
      m = owner.instance_method(meth)
      return false unless m.source_location # C methods can't be targeted; maybe a later reopen defines it
      TracePoint.new(:call, :line) { |tp| safely(key) { tp.event == :call ? handler.call(tp) : first_line(tp) } }.enable(target: m)
      @armed[key] = [const_name, meth, singleton, nil] # done
      true
    rescue StandardError, ScriptError => e
      log_error("arm #{key}", e)
      false
    end

    def resolve_const(name)
      name.split("::").reject(&:empty?).inject(Object) do |mod, part|
        return nil unless mod.const_defined?(part, false) && !mod.autoload?(part)
        mod.const_get(part, false)
      end
    end

    # file:line => the method whose body contains that line, found by walking the compiled ISeq tree
    # (compiling does not execute anything or activate gems).
    def arm_file_line(abs, line)
      found = nil
      walk = lambda do |iseq, nesting|
        label = iseq.label
        nesting += [Regexp.last_match(1)] if label =~ /\A<(?:class|module):(.+)>\z/
        if iseq.to_a[9] == :method && iseq.trace_points.any? { |l, ev| ev == :line && l == line }
          found = [nesting.join("::"), label.to_sym]
        end
        iseq.each_child { |c| walk.call(c, nesting) }
      end
      walk.call(RubyVM::InstructionSequence.compile_file(abs), [])
      unless found
        log_error("start_at", ArgumentError.new("no method contains #{abs}:#{line}; tracing whole program"))
        return @main.enable
      end
      const = found[0].empty? ? "Object" : found[0]
      arm(const, found[1], false) { |tp| start_window(tp) }
      arm(const, found[1], true) { |tp| start_window(tp) }
    rescue StandardError, ScriptError => e
      log_error("arm_file_line", e)
    end

    # Default for test runners: one window per test body, setup excluded.
    def arm_tests
      # RSpec: once specs are loaded, an around hook arms a TracePoint targeted at each example block.
      probe = self # the around block is instance_exec'd on the example: never call probe methods implicitly
      arm("RSpec::Core::Runner", :run_specs, false) do |_tp|
        RSpec.configure do |c|
          c.around(:example) do |ex|
            tp = probe.safely("rspec arm") do
              blk = ex.metadata[:block] || ex.example.instance_variable_get(:@example_block)
              blk && TracePoint.new(:b_call, :line) do |t|
                probe.safely("rspec example") { t.event == :b_call ? probe.start_window(t, "example: #{ex.full_description}") : probe.first_line(t) }
              end.tap { |x| x.enable(target: blk) }
            end
            begin
              ex.run
            ensure
              probe.safely("rspec disarm") { tp&.disable }
            end
          end
        end
      end
      # Minitest (and Rails' ActiveSupport::TestCase): arm the test method itself inside #run.
      arm("Minitest::Test", :run, false) do |tp|
        test = tp.self
        name = test.respond_to?(:name) ? test.name : nil
        m = name && test.method(name)
        next unless m && project?(m.source_location&.first.to_s)

        once = TracePoint.new(:call, :line) { |t| safely("minitest test") { t.event == :call ? start_window(t) : first_line(t) } }
        once.enable(target: m)
        (@pending_test_tps ||= []) << once
      end
    end

    # ---------------------------------------------------------------- windows
    def start_window(tp, label = nil)
      return if @main.enabled? || @count >= @max_events
      @window_label = label
      subscribe_sql
      Thread.current[:__visualdebug_stack] = []
      @window_thread = Thread.current
      @window_root = true
      @main.enable(target_thread: Thread.current)
      emit_meta(kind: "window_start")
      on_event(tp)
      @first_line_pending = true
    rescue StandardError, ScriptError => e
      log_error("start_window", e)
    end

    def first_line(tp)
      return unless @first_line_pending && Thread.current == @window_thread
      @first_line_pending = false
      on_event(tp)
    end

    def stop_window
      @main.disable
      @window_root = false
      emit_meta(kind: "window_end")
      flush
    end

    # ---------------------------------------------------------------- events
    def stack = (Thread.current[:__visualdebug_stack] ||= [])

    def abs(path)
      @path_cache[path] ||= File.expand_path(path)
    end

    def project?(path)
      v = @project_cache&.[](path)
      return v unless v.nil?
      return (@project_cache ||= {})[path] = false if path.start_with?("<", "(") # <internal:kernel>, (eval)
      a = abs(path)
      v = a.start_with?(@root) && a != SELF_PATH && !a.include?("/vendor/") && !a.include?("/node_modules/") &&
          !a.include?("/.bundle/") && !a.start_with?("#{@root}tmp/") &&
          (!a.start_with?("#{@root}.visualdebug/") || a.start_with?("#{@root}.visualdebug/examples/"))
      (@project_cache ||= {})[path] = v
    end

    def rel(path) = abs(path).delete_prefix(@root)

    def line_level?(rel_path) = @line_files.nil? || @line_files.key?(rel_path)

    # Frames on our stack are project frames only: [frame_id, name, raised?, method_id, path, line_level?]
    # Library frames are never pushed — Ruby re-fires :call on `retry` with no matching :return, so a
    # push/pop count over library code drifts. Library calls classify themselves via caller_locations.
    # A library call is recorded when made directly by the current project frame (not by a class body
    # running during autoload). Skip our own frames; the next is the traced method, then its caller.
    def traced_caller
      locs = caller_locations(1, 14) or return nil
      i = locs.index { |l| l.path != SELF_PATH } or return nil
      locs[i + 1]
    end

    def called_from_top?(top)
      (c = traced_caller) && c.path == top[4]
    end

    # Load/definition hooks: real calls, but noise when reading a trace.
    HOOKS = %i[require require_relative load autoload const_added inherited method_added included extended prepended].freeze

    def on_event(tp)
      return if Thread.current[:__visualdebug_busy]
      path = tp.path
      st = stack
      case tp.event
      when :call, :b_call
        if project?(path)
          top = st.last
          # `retry` re-fires :call for the frame already on top. Recursion looks the same, except a
          # recursive call's caller is the method itself.
          if tp.event == :call && top && top[3] == tp.method_id && top[4] == path
            c = traced_caller
            return unless c && c.base_label == tp.method_id.to_s
          end
          id = (@frame_seq += 1)
          name = tp.event == :b_call ? block_name(tp) : fn_name(tp)
          r = rel(path)
          ll = line_level?(r) || (st.empty? && @window_root) # the window's root (e.g. a test body) is context
          st << [id, name, false, tp.method_id, path, ll]
          emit(event: tp.event == :call ? "call" : "block_call", file: r, line: tp.lineno, function: name,
               frame_id: id, depth: st.size, locals: ll || tp.event == :call ? locals(tp.binding) : nil)
        elsif tp.event == :call && st.last && !HOOKS.include?(tp.method_id) && called_from_top?(st.last)
          emit(event: "call", library: true, function: fn_name(tp), frame_id: st.last[0], depth: st.size + 1)
        end
      when :return, :b_return
        if project?(path)
          i = st.rindex { |f| f[4] == path && f[3] == tp.method_id } or return
          fr = st[i]
          st.slice!(i..) # also drops frames skipped by non-local exits
          emit(event: tp.event == :return ? "return" : "block_return", file: rel(path), line: tp.lineno,
               function: fr[1], frame_id: fr[0], depth: st.size + 1, return_value: ser(tp.return_value))
          stop_window if st.empty? && @window_root
        elsif tp.event == :return && st.last && !HOOKS.include?(tp.method_id) && called_from_top?(st.last)
          emit(event: "return", library: true, function: fn_name(tp), frame_id: st.last[0], depth: st.size + 1,
               return_value: ser(tp.return_value))
        end
      when :line
        top = st.last
        if top.nil? && !@window_root && @main_script && abs(path) == @main_script
          # Top-level code of the script being run: give it a frame (other files' bodies stay unrecorded).
          id = (@frame_seq += 1)
          st << (top = [id, "<main>", false, nil, path, line_level?(rel(path))])
          emit(event: "call", file: rel(path), line: tp.lineno, function: "<main>", frame_id: id, depth: 1)
        end
        return unless top && path == top[4] # class bodies run during autoload have no frame of their own
        @first_line_pending = false
        top[2] = false
        return unless top[5]
        emit(event: "line", file: rel(path), line: tp.lineno, function: top[1], frame_id: top[0], depth: st.size,
             locals: locals(tp.binding))
      when :raise
        top = st.last
        return unless top && project?(path)
        top[2] = true
        e = tp.raised_exception
        emit(event: "raise", file: rel(path), line: tp.lineno, function: top[1], frame_id: top[0], depth: st.size,
             exception: { type: class_of(e).name.to_s, message: cut(safe_message(e)) })
      end
    rescue StandardError, ScriptError, SystemStackError => e
      log_error("on_event", e)
    end

    def block_name(tp)
      if (label = @window_label) && stack.empty?
        @window_label = nil
        return label
      end
      fn = fn_name(tp)
      fn.empty? ? "block in #{File.basename(tp.path)}" : "block in #{fn}"
    end

    def safe_message(e)
      Exception.instance_method(:message).bind_call(e).to_s
    rescue StandardError
      "?"
    end

    def fn_name(tp)
      dc = tp.defined_class
      return tp.method_id.to_s unless dc
      if dc.singleton_class?
        obj = dc.respond_to?(:attached_object) ? (dc.attached_object rescue nil) : nil
        "#{obj.is_a?(Module) ? obj.name : dc.inspect}.#{tp.method_id}"
      else
        "#{dc.name || dc.inspect}##{tp.method_id}"
      end
    end

    # ---------------------------------------------------------------- output
    def emit(ev)
      if @count >= @max_events
        unless @truncated
          @truncated = true
          @buf << stamp(event: "meta", kind: "truncated")
          @main.disable
          flush
        end
        return
      end
      @count += 1
      ev.delete(:locals) if ev[:locals].nil?
      @buf << stamp(ev)
      flush if @buf.size >= 500
    end

    def emit_meta(**kw) = (@buf << stamp(event: "meta", **kw))

    def stamp(ev)
      ev[:seq] = (@seq += 1)
      ev[:t] = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)
      ev[:pid] = Process.pid
      ev[:tid] = Thread.current.respond_to?(:native_thread_id) ? Thread.current.native_thread_id : Thread.current.object_id
      ev
    end

    def flush
      return if @buf.empty?
      if @io_pid != Process.pid # reopen after fork
        @io_pid = Process.pid
        @io = File.open(File.join(@out, "trace-#{@io_pid}.jsonl"), "a")
      end
      @buf.each { |e| @io.write(JSON.generate(e), "\n") }
      @io.flush
      @buf.clear
    rescue StandardError => e
      @buf.clear
      log_error("flush", e)
    end

    def log_error(where, e)
      File.open(File.join(@out || ".", "probe-errors.log"), "a") do |f|
        f.puts("#{Process.pid} ruby #{where}: #{e.class}: #{e.message} #{e.backtrace&.first(3)&.join(' | ')}")
      end
    rescue StandardError
      nil
    end

    # ---------------------------------------------------------------- SQL
    def subscribe_sql
      return if @sql_sub
      as = resolve_const("ActiveSupport::Notifications") or return
      @sql_sub = as.subscribe("sql.active_record") do |*args|
        next unless @main.enabled? && Thread.current == @window_thread
        next if args[4].is_a?(Hash) && args[4][:name] == "SCHEMA" # Rails introspection, not the app's queries
        Thread.current[:__visualdebug_busy] = true
        begin
          _name, start, finish, _id, payload = args
          top = stack.last
          binds = payload[:type_casted_binds]
          binds = [] unless binds.is_a?(Array)
          emit(event: "side_effect", frame_id: top&.first, depth: stack.size, function: top&.[](1),
               side_effect: { kind: "sql", sql: cut(payload[:sql].to_s), name: payload[:name],
                              duration_ms: ((finish - start) * 1000).round(3),
                              binds: binds.first(@max_items).map { |x| ser(x)[:repr] } })
        rescue StandardError => e
          log_error("sql", e)
        ensure
          Thread.current[:__visualdebug_busy] = false
        end
      end
    rescue StandardError, ScriptError => e
      log_error("subscribe_sql", e)
    end

    # ---------------------------------------------------------------- serialization
    # Never calls code that could do I/O: no Relation#inspect, no library-defined #inspect.
    K_CLASS = Kernel.instance_method(:class)
    K_OBJECT_ID = Kernel.instance_method(:object_id)
    K_METHOD = Kernel.instance_method(:method)
    K_IVARS = Kernel.instance_method(:instance_variables)
    K_IVAR_GET = Kernel.instance_method(:instance_variable_get)
    IMMEDIATE = [NilClass, TrueClass, FalseClass, Integer, Float, Symbol].freeze
    REDACTED = { type: "redacted", repr: "[REDACTED]", truncated: false }.freeze

    def class_of(v) = K_CLASS.bind_call(v)

    def cut(s)
      s.length > @max_repr ? s[0, @max_repr] + "…" : s
    end

    def redact(v)
      klass = class_of(v)
      unless IMMEDIATE.include?(klass)
        @redacted_ids.shift if @redacted_ids.size > 10_000
        @redacted_ids[K_OBJECT_ID.bind_call(v)] = true
      end
      REDACTED
    end

    def ser(v)
      klass = class_of(v)
      return REDACTED if !IMMEDIATE.include?(klass) && @redacted_ids.key?(K_OBJECT_ID.bind_call(v))
      @cut_off = false
      r = repr(v, klass, 0)
      out = { type: klass.name || klass.to_s, repr: cut(r), truncated: @cut_off || r.length > @max_repr }
      out[:id] = K_OBJECT_ID.bind_call(v) unless IMMEDIATE.include?(klass) || klass == String
      out
    rescue StandardError, ScriptError, SystemStackError
      { type: (klass.name rescue "?"), repr: "#<#{klass rescue '?'} (repr failed)>", truncated: false }
    end

    def ar_classes
      @ar_classes ||= begin
        base = resolve_const("ActiveRecord::Base")
        rel = resolve_const("ActiveRecord::Relation")
        base && rel ? [base, rel] : nil
      end
    end

    def repr(v, klass, depth)
      case v
      when *IMMEDIATE then v.inspect
      when String then v.length > @max_repr ? (@cut_off = true; v[0, @max_repr].inspect) : v.inspect
      when Array
        return (@cut_off = true; "[…]") if depth >= @max_depth
        items = v.first(@max_items).map { |x| repr(x, class_of(x), depth + 1) }
        (@cut_off = true; items << "…(#{v.size - @max_items} more)") if v.size > @max_items
        "[#{items.join(', ')}]"
      when Hash
        return (@cut_off = true; "{…}") if depth >= @max_depth
        items = v.first(@max_items).map do |k, x|
          key = k.is_a?(Symbol) ? "#{k}:" : "#{repr(k, class_of(k), depth + 1)} =>"
          "#{key} #{@redact.match?(k.to_s) ? '[REDACTED]' : repr(x, class_of(x), depth + 1)}"
        end
        (@cut_off = true; items << "…") if v.size > @max_items
        "{#{items.join(', ')}}"
      else
        base, rel = ar_classes
        if rel && rel === v
          # Relation#inspect loads records: never call it.
          v.loaded? ? "#<#{klass.name} #{v.model.name} loaded (#{v.size})>" : "#<#{klass.name} #{v.model.name} unloaded>"
        elsif base && base === v
          # Attributes only (no associations, no default #inspect).
          attrs = v.attributes.first(8).map do |k, x|
            "#{k}: #{@redact.match?(k) ? '[REDACTED]' : repr(x, class_of(x), @max_depth)}"
          end
          flags = []
          flags << "new" if v.new_record?
          flags << "changed=#{v.changed.join(',')}" if v.changed?
          "#<#{klass.name} #{attrs.join(', ')}#{flags.map { |f| " (#{f})" }.join}>"
        elsif klass.name == "BigDecimal"
          v.to_s("F")
        elsif v.is_a?(Struct)
          return "#<struct #{klass.name}>" if depth >= @max_depth
          body = v.each_pair.first(@max_items).map do |k, x|
            "#{k}=#{@redact.match?(k.to_s) ? '[REDACTED]' : repr(x, class_of(x), depth + 1)}"
          end
          "#<struct #{klass.name} #{body.join(', ')}>"
        else
          safe_inspect(v, klass, depth)
        end
      end
    end

    # A class's own #inspect only if C-implemented (core/stdlib) or project-defined; a library's Ruby
    # #inspect may do I/O, so render those from ivars ourselves.
    # Library classes whose #inspect is pure formatting.
    SAFE_INSPECT = %w[ActiveSupport::TimeWithZone ActiveSupport::Duration ActiveSupport::SafeBuffer Date DateTime Time
                      Pathname URI::HTTP URI::HTTPS URI::Generic IPAddr Set Rational Complex].freeze

    def safe_inspect(v, klass, depth)
      return v.inspect if SAFE_INSPECT.include?(klass.name)
      m = K_METHOD.bind_call(v, :inspect)
      return ivars_repr(v, klass, depth) if m.owner == Kernel # default #inspect: ours redacts and omits addresses
      loc = m.source_location
      return v.inspect if loc.nil? || project?(loc[0].to_s)
      ivars_repr(v, klass, depth)
    rescue NameError
      ivars_repr(v, klass, depth)
    end

    def ivars_repr(v, klass, depth)
      name = klass.name || klass.to_s
      return "#<#{name}>" if depth >= @max_depth
      names = K_IVARS.bind_call(v).reject { |n| n.start_with?("@_") }
      return "#<#{name}>" if names.empty?
      body = names.first(@max_items).map do |n|
        x = K_IVAR_GET.bind_call(v, n)
        "#{n}=#{@redact.match?(n.to_s) ? '[REDACTED]' : repr(x, class_of(x), depth + 1)}"
      end
      "#<#{name} #{body.join(', ')}>"
    end

    # Set by RSpec/Minitest/Rails test helpers on every test instance.
    FRAMEWORK_IVARS = %i[@html_document @controller @request @response @integration_session @routes @url_options
                         @app @fixture_cache @fixture_cache_key @fixture_connections @loaded_fixtures @connection_subscriber
                         @saved_pool_configs @legacy_saved_pool_configs @matcher_definitions @__memoized @__inspect_output
                         @example @assertions @NAME @name @failures @time @metadata @mock_proxies @tagged_logger
                         @middleware @_routes @named_routes @default_url_options @rendered @view_flow
                         @output_buffer @virtual_path @method_name @RSpec_example_instance].freeze

    def locals(b)
      out = {}
      b.local_variables.each { |n| v = b.local_variable_get(n); out[n] = @redact.match?(n.to_s) ? redact(v) : ser(v) }
      slf = b.receiver
      unless IMMEDIATE.include?(class_of(slf)) || slf.is_a?(Module)
        K_IVARS.bind_call(slf).each do |n|
          next if n.start_with?("@_") || FRAMEWORK_IVARS.include?(n)
          break if out.size >= 40
          v = K_IVAR_GET.bind_call(slf, n)
          out[n] = @redact.match?(n.to_s) ? redact(v) : ser(v)
        end
      end
      out
    end
  end
end

if (path = ENV["VISUALDEBUG_CONFIG"]) && !defined?(VISUALDEBUG_PROBE_LOADED)
  VISUALDEBUG_PROBE_LOADED = true
  begin
    VisualdebugProbe.install(JSON.parse(File.read(path)))
  rescue StandardError, ScriptError => e
    warn "visualdebug: probe disabled (#{e.class}: #{e.message})"
  end
end
