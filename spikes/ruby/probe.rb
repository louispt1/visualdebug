# Phase 0 spike — throwaway. Injected via RUBYOPT=-r<this file>.
# Proves: injection under bundle exec rspec, safe locals capture, SQL attached to the right step,
# --start-at via a cheap :end hook (near-zero overhead before the window).
require "json"

module VisualdebugSpike
  CFG = JSON.parse(File.read(ENV.fetch("VISUALDEBUG_CONFIG")))
  ROOT = CFG.fetch("root")
  OUT = CFG.fetch("out_dir")
  ERR_LOG = File.join(OUT, "probe-errors.log")
  SELF_PATH = File.expand_path(__FILE__)
  FOCUS = CFG["focus"] # nil => every project file gets line-level detail
  MAX_EVENTS = CFG.fetch("max_events", 20_000)
  START_CLASS, START_METHOD = CFG["start_at"]&.split("#")
  REDACT = Regexp.new(CFG.fetch("redact", "password|passwd|secret|token|api_key|apikey|authorization|cookie|session|private_key|credential"), Regexp::IGNORECASE)
  MAX_REPR = 200
  MAX_ITEMS = 10
  MAX_DEPTH = 3
  K_CLASS = Kernel.instance_method(:class)
  K_INSPECT = Kernel.instance_method(:inspect)
  K_OBJECT_ID = Kernel.instance_method(:object_id)
  IMMEDIATE = [NilClass, TrueClass, FalseClass, Integer, Float, Symbol].freeze

  @seq = 0
  @count = 0
  @buf = []
  @pid = nil
  @io = nil
  @frame_seq = 0
  @truncated = false

  class << self
    def log_error(where, e)
      File.open(ERR_LOG, "a") { |f| f.puts("#{Process.pid} #{where}: #{e.class}: #{e.message} #{e.backtrace&.first(3)&.join(' | ')}") }
    rescue StandardError
      nil
    end

    def project?(path)
      path.start_with?(ROOT) && path != SELF_PATH &&
        !path.include?("/vendor/") && !path.include?("/node_modules/") && !path.include?("/tmp/")
    end

    def focus_line?(rel, line)
      return true if FOCUS.nil?
      ranges = FOCUS[rel] or return false
      ranges.any? { |a, b| line >= a && line <= b }
    end

    def rel(path) = path.delete_prefix(ROOT + "/")

    def emit(ev)
      if @count >= MAX_EVENTS
        @truncated ||= (@buf << { event: "meta", truncated: true }; true)
        return
      end
      @count += 1
      ev[:seq] = (@seq += 1)
      ev[:t] = Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond)
      ev[:pid] = Process.pid
      ev[:tid] = Thread.current.native_thread_id
      @buf << ev
      flush if @buf.size >= 500
    end

    def flush
      return if @buf.empty?
      if @pid != Process.pid # reopen after fork
        @pid = Process.pid
        @io = File.open(File.join(OUT, "trace-#{@pid}.jsonl"), "a")
      end
      @buf.each { |e| @io.write(JSON.generate(e), "\n") }
      @io.flush
      @buf.clear
    end

    # ---- serialization: must never call into user/lib code that does I/O ----
    def const_loaded(mod, name)
      mod.const_defined?(name, false) && !mod.autoload?(name) ? mod.const_get(name, false) : nil
    end

    def ar_classes
      @ar ||= begin
        ar = const_loaded(Object, :ActiveRecord)
        ar && const_loaded(ar, :Base) && const_loaded(ar, :Relation) && [ar::Base, ar::Relation]
      end
    end

    def cut(s)
      s.length > MAX_REPR ? [s[0, MAX_REPR] + "…", true] : [s, false]
    end

    def ser(v, depth = 0)
      klass = K_CLASS.bind_call(v)
      out = { type: klass.name || klass.to_s }
      repr, trunc = cut(repr_of(v, klass, depth))
      out[:repr] = repr
      out[:truncated] = trunc || @trunc_flag == true
      out[:id] = K_OBJECT_ID.bind_call(v) unless IMMEDIATE.include?(klass)
      out
    rescue StandardError => e
      { type: (klass&.name rescue "?"), repr: "#<#{klass} (repr failed)>", truncated: false }
    end

    def repr_of(v, klass, depth)
      @trunc_flag = false
      ar_base, ar_rel = ar_classes
      case v
      when *IMMEDIATE then v.inspect
      when String then v.inspect
      when Array
        return "[…]" if depth >= MAX_DEPTH
        items = v.first(MAX_ITEMS).map { |x| repr_of(x, K_CLASS.bind_call(x), depth + 1) }
        items << "…(#{v.size - MAX_ITEMS} more)" if v.size > MAX_ITEMS
        "[#{items.join(', ')}]"
      when Hash
        return "{…}" if depth >= MAX_DEPTH
        items = v.first(MAX_ITEMS).map do |k, x|
          kr = k.is_a?(Symbol) ? "#{k}:" : "#{k.inspect} =>"
          val = REDACT.match?(k.to_s) ? "[REDACTED]" : repr_of(x, K_CLASS.bind_call(x), depth + 1)
          "#{kr} #{val}"
        end
        items << "…" if v.size > MAX_ITEMS
        "{#{items.join(', ')}}"
      else
        if ar_rel && ar_rel === v
          # Relation#inspect loads records — never call it.
          v.loaded? ? "#<#{klass.name} #{v.model.name} loaded (#{v.size})>" : "#<#{klass.name} #{v.model.name} unloaded>"
        elsif ar_base && ar_base === v
          attrs = v.attributes.first(6).map { |k, x| "#{k}: #{REDACT.match?(k) ? '[REDACTED]' : x.inspect}" }.join(", ")
          flags = []
          flags << "new" if v.new_record?
          flags << "changed=#{v.changed.join(',')}" if v.changed?
          "#<#{klass.name} id=#{v.id.inspect}#{flags.empty? ? '' : ' ' + flags.join(' ')} #{attrs}>"
        else
          safe_inspect(v, klass, depth)
        end
      end
    end

    K_METHOD = Kernel.instance_method(:method)
    K_IVARS = Kernel.instance_method(:instance_variables)
    K_IVAR_GET = Kernel.instance_method(:instance_variable_get)

    # Own inspect only if C-implemented (core/stdlib) or project-defined; library Ruby inspect may do I/O.
    def safe_inspect(v, klass, depth)
      loc = K_METHOD.bind_call(v, :inspect).source_location
      return v.inspect if loc.nil? || project?(loc[0])
      ivars(v, klass, depth)
    rescue NameError
      ivars(v, klass, depth)
    end

    def ivars(v, klass, depth)
      return "#<#{klass}>" if depth >= MAX_DEPTH
      names = K_IVARS.bind_call(v)
      return "#<#{klass}>" if names.empty?
      body = names.first(MAX_ITEMS).map { |n| x = K_IVAR_GET.bind_call(v, n); "#{n}=#{repr_of(x, K_CLASS.bind_call(x), depth + 1)}" }
      "#<#{klass} #{body.join(", ")}>"
    end

    def locals(b, slf = b.receiver)
      vars = b.local_variables.to_h { |n| [n, b.local_variable_get(n)] }
      K_IVARS.bind_call(slf).first(MAX_ITEMS).each { |n| vars[n] = K_IVAR_GET.bind_call(slf, n) } unless IMMEDIATE.include?(K_CLASS.bind_call(slf))
      vars.to_h do |n, val|
        [n, REDACT.match?(n.to_s) ? { type: "redacted", repr: "[REDACTED]", truncated: false } : ser(val)]
      end
    end

    def fn_name(tp)
      dc = tp.defined_class
      return tp.method_id.to_s unless dc
      if dc.singleton_class?
        "#{(dc.attached_object rescue dc).inspect}.#{tp.method_id}"
      else
        "#{dc.name}##{tp.method_id}"
      end
    end

    # ---- the window ----
    def stack = (Thread.current[:__vd_stack] ||= [])

    # Only project frames live on the stack. Library frames are never pushed: Ruby fires :call again on
    # `retry` with no matching :return (Arel does this), so counting library push/pop drifts.
    # Instead each library call/return asks "is my caller project code?" via caller_locations.
    def caller_project?
      loc = caller_locations(4, 1)&.first # 0=here 1=on_event 2=hook block 3=traced method 4=its caller
      loc && project?(loc.path.to_s)
    end

    def on_event(tp)
      return if Thread.current[:__vd_busy]
      path = tp.path
      st = stack
      case tp.event
      when :call, :b_call
        if project?(path)
          top = st.last
          # retry re-fires :call for the frame already on top, after a :raise in it.
          return if tp.event == :call && top && top[2] && top[3] == tp.method_id && top[4] == path
          id = (@frame_seq += 1)
          name = tp.event == :b_call ? "block in #{fn_name(tp)}" : fn_name(tp)
          st << [id, name, false, tp.method_id, path]
          r = rel(path)
          ev = { event: tp.event == :call ? "call" : "block_call", file: r, line: tp.lineno, function: name, frame_id: id, depth: st.size }
          ev[:locals] = locals(tp.binding) if focus_line?(r, tp.lineno)
          emit(ev)
        elsif tp.event == :call && caller_project?
          emit(event: "call", library: true, function: fn_name(tp), frame_id: st.last&.first, depth: st.size + 1)
        end
      when :return, :b_return
        if project?(path)
          i = st.rindex { |f| f[4] == path && f[3] == tp.method_id } or return
          fr = st[i]
          st.slice!(i..) # also drops frames skipped by non-local exits
          emit(event: tp.event == :return ? "return" : "block_return", file: rel(path), line: tp.lineno,
               function: fr[1], frame_id: fr[0], depth: st.size + 1, return_value: ser(tp.return_value))
          stop_window if st.empty? && START_CLASS
        elsif tp.event == :return && caller_project?
          emit(event: "return", library: true, function: fn_name(tp), frame_id: st.last&.first, depth: st.size + 1,
               return_value: ser(tp.return_value))
        end
      when :line
        top = st.last
        return unless top && path == top[4] # class bodies run during autoload have no frame of their own
        @first_line_pending = false
        top[2] = false
        r = rel(path)
        return unless focus_line?(r, tp.lineno)
        emit(event: "line", file: r, line: tp.lineno, function: top[1], frame_id: top[0], depth: st.size, locals: locals(tp.binding))
      when :raise
        top = st.last
        return unless top && project?(path)
        top[2] = true
        e = tp.raised_exception
        emit(event: "raise", file: project?(path) ? rel(path) : nil, line: tp.lineno, frame_id: top[0], depth: st.size,
             exception: { type: K_CLASS.bind_call(e).name, message: cut((e.message rescue "?").to_s)[0] })
      end
    rescue StandardError, ScriptError, SystemStackError => e
      log_error("on_event", e)
    end

    def start_window(tp)
      return if @main.enabled?
      subscribe_sql
      Thread.current[:__vd_stack] = []
      @window_thread = Thread.current
      # Synthesize the entry call (the main tracepoint isn't on yet for it), then trace everything until it returns.
      @main.enable(target_thread: Thread.current)
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
      flush
    end

    def subscribe_sql
      return if @sql_sub
      as = const_loaded(Object, :ActiveSupport) or return
      notif = const_loaded(as, :Notifications) or return
      @sql_sub = notif.subscribe("sql.active_record") do |*args|
        next unless @main.enabled? && Thread.current == @window_thread
        Thread.current[:__vd_busy] = true
        begin
          _name, start, finish, _id, payload = args
          top = stack.last
          emit(event: "side_effect", frame_id: top&.first, depth: stack.size,
               side_effect: { kind: "sql", sql: cut(payload[:sql].to_s)[0], name: payload[:name],
                              duration_ms: ((finish - start) * 1000).round(3),
                              binds: Array(payload[:type_casted_binds].is_a?(Proc) ? [] : payload[:type_casted_binds]).first(MAX_ITEMS).map { |x| ser(x)[:repr] } })
        rescue StandardError => e
          log_error("sql", e)
        ensure
          Thread.current[:__vd_busy] = false
        end
      end
    end

    def install
      Dir.mkdir(OUT) unless Dir.exist?(OUT)
      @main = TracePoint.new(:call, :return, :b_call, :b_return, :line, :raise) { |tp| on_event(tp) }
      if START_CLASS
        # :end fires once per class/module body — cheap. When the target method exists, arm a
        # TracePoint scoped to that one method (zero cost for every other call).
        @arm = TracePoint.new(:end) do |tp|
          next unless (tp.self.name rescue nil) == START_CLASS && tp.self.method_defined?(START_METHOD, false)
          m = tp.self.instance_method(START_METHOD)
          # :line too: a TracePoint enabled inside a :call hook misses the first line (shared instruction).
          TracePoint.new(:call, :line) { |t| t.event == :call ? start_window(t) : first_line(t) }.enable(target: m)
          @arm.disable
        rescue StandardError, ScriptError => e
          log_error("arm", e)
        end
        @arm.enable
      else
        @main.enable
      end
      at_exit { (@main.disable; flush) rescue nil }
    end
  end

  install
end
