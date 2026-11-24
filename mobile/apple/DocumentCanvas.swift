import MetalKit
import UIKit
import os

/// The engine's side of a canvas: it calls `frameReady` on its own thread.
/// The canvas is set on the main thread and read there only.
final class FrameSignal: DocumentObserver, @unchecked Sendable {
  weak var canvas: DocumentCanvas?

  func frameReady() {
    DispatchQueue.main.async { [weak self] in self?.canvas?.setNeedsDisplay() }
  }
}

/// Draws a document's frames and sends it the user's touches and typing.
final class DocumentCanvas: MTKView {
  private let document: Document
  private let painter: MetalPainter
  /// Each frame, for the chrome: its state, menus and the rest.
  var onFrame: ((Frame) -> Void)?
  private var sent: Viewport?
  private let proxy = TextProxy()
  private var controls: [UInt64: CanvasElement] = [:]
  private var presented: [Element] = []
  private static let log = Logger(subsystem: "dev.openmaker.go", category: "frames")

  init(document: Document, painter: MetalPainter) {
    self.document = document
    self.painter = painter
    super.init(frame: .zero, device: painter.device)
    colorPixelFormat = MetalPainter.pixelFormat
    framebufferOnly = true
    // Drawn when the engine has a frame, not on a timer.
    isPaused = true
    enableSetNeedsDisplay = true
    isMultipleTouchEnabled = true
    accessibilityLabel = "FileMaker layout"
    proxy.send = { [weak self] input in self?.document.send(input: input) }
    addSubview(proxy)

    let tap = UITapGestureRecognizer(target: self, action: #selector(tapped))
    addGestureRecognizer(tap)
    let pan = UIPanGestureRecognizer(target: self, action: #selector(panned))
    pan.maximumNumberOfTouches = 1
    addGestureRecognizer(pan)
    // FileMaker Go: swipe with two fingers to go through the records.
    for direction: UISwipeGestureRecognizer.Direction in [.left, .right] {
      let swipe = UISwipeGestureRecognizer(target: self, action: #selector(swiped))
      swipe.direction = direction
      swipe.numberOfTouchesRequired = 2
      addGestureRecognizer(swipe)
    }
  }

  required init(coder: NSCoder) { fatalError("not used from a storyboard") }

  override func layoutSubviews() {
    super.layoutSubviews()
    sendViewport()
  }

  override func didMoveToWindow() {
    super.didMoveToWindow()
    sendViewport()
    // A frame may have come before the canvas was shown.
    setNeedsDisplay()
    // A hardware keyboard's keys reach the layout before any field is
    // entered (Tab to the first field).
    if window != nil { becomeFirstResponder() }
  }

  private func sendViewport() {
    guard bounds.width >= 1, bounds.height >= 1, window != nil else { return }
    let viewport = Viewport(
      width: Float(bounds.width), height: Float(bounds.height), scale: Float(contentScaleFactor),
      maxTextureSide: 8192)
    guard viewport != sent else { return }
    sent = viewport
    document.send(input: .resize(viewport: viewport))
  }

  override func draw(_ rect: CGRect) {
    if let frame = document.takeFrame() {
      #if DEBUG
        let timings = frame.timings
        if timings.pass + timings.tessellate + timings.pack > 16_000 {
          Self.log.notice(
            "slow frame: pass \(timings.pass) µs, tessellate \(timings.tessellate) µs, pack \(timings.pack) µs"
          )
        }
      #endif
      painter.take(frame)
      if let elements = frame.elements { present(elements) }
      onFrame?(frame)
    }
    painter.draw(in: self)
  }

  // MARK: Touch

  @objc private func tapped(_ gesture: UITapGestureRecognizer) {
    let point = gesture.location(in: self)
    // A tap can move the focus to another field: marked text is committed
    // in its own field first.
    proxy.settle(then: .tap(x: Float(point.x), y: Float(point.y)))
  }

  @objc private func panned(_ gesture: UIPanGestureRecognizer) {
    let moved = gesture.translation(in: self)
    gesture.setTranslation(.zero, in: self)
    switch gesture.state {
    case .began:
      // Where the finger went down: what is there scrolls.
      let at = gesture.location(in: self)
      document.send(input: .panBegan(x: Float(at.x - moved.x), y: Float(at.y - moved.y)))
      document.send(input: .panMoved(dx: Float(moved.x), dy: Float(moved.y)))
    case .changed:
      document.send(input: .panMoved(dx: Float(moved.x), dy: Float(moved.y)))
    case .ended:
      let velocity = gesture.velocity(in: self)
      document.send(input: .panEnded(vx: Float(velocity.x), vy: Float(velocity.y)))
    default:
      document.send(input: .panEnded(vx: 0, vy: 0))
    }
  }

  @objc private func swiped(_ gesture: UISwipeGestureRecognizer) {
    let command: Command = gesture.direction == .left ? .next : .previous
    proxy.settle(then: .run(command: command))
  }

  /// Send `input`, a command of the chrome or a menu choice, after the
  /// input method's marked text is committed in its field: the engine and
  /// the text field then agree that nothing is composing.
  func settle(then input: Input) {
    proxy.settle(then: input)
  }

  // MARK: Keyboard

  /// Show or hide the keyboard as the engine's field focus says, with the
  /// input method's marked text at the field's insertion point.
  func keyboard(shown: Bool, caret: Caret?, finding: Bool) {
    proxy.finding = finding
    proxy.password = caret?.password == true
    if let caret {
      proxy.frame = CGRect(
        x: CGFloat(caret.x), y: CGFloat(caret.y), width: max(CGFloat(caret.width), 1),
        height: max(CGFloat(caret.height), 1))
    }
    if shown, !proxy.isFirstResponder {
      proxy.becomeFirstResponder()
    } else if !shown, proxy.isFirstResponder {
      proxy.leave()
      // A hardware keyboard's Tab and arrows still reach the layout.
      becomeFirstResponder()
    }
  }

  override var canBecomeFirstResponder: Bool { true }

  /// A hardware keyboard's keys when no field takes typing (Tab to the
  /// first field).
  override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
    var unhandled = Set<UIPress>()
    for press in presses {
      if let input = press.key.flatMap(TextProxy.input(for:)) {
        document.send(input: input)
      } else {
        unhandled.insert(press)
      }
    }
    if !unhandled.isEmpty { super.pressesBegan(unhandled, with: event) }
  }

  // MARK: Accessibility

  /// VoiceOver's three-finger scroll pages what is under the focused element
  /// (a portal, a Table view, the layout), as a pan there does: a swipe up
  /// brings what is below into view, a swipe left what is to the right.
  override func accessibilityScroll(_ direction: UIAccessibilityScrollDirection) -> Bool {
    let (width, height) = (bounds.width * 0.8, bounds.height * 0.8)
    let (dx, dy): (CGFloat, CGFloat)
    switch direction {
    case .up: (dx, dy) = (0, -height)
    case .down: (dx, dy) = (0, height)
    case .left: (dx, dy) = (-width, 0)
    case .right: (dx, dy) = (width, 0)
    default: return false
    }
    let focused = UIAccessibility.focusedElement(using: .notificationVoiceOver) as? CanvasElement
    let at = focused.map {
      CGPoint(x: $0.accessibilityFrameInContainerSpace.midX, y: $0.accessibilityFrameInContainerSpace.midY)
    } ?? CGPoint(x: bounds.midX, y: bounds.midY)
    document.send(input: .panBegan(x: Float(at.x), y: Float(at.y)))
    document.send(input: .panMoved(dx: Float(dx), dy: Float(dy)))
    document.send(input: .panEnded(vx: 0, vy: 0))
    // Told once the elements the scroll shows are presented.
    scrolledByVoiceOver = true
    return true
  }

  /// VoiceOver asked for a scroll whose elements are yet to be presented.
  private var scrolledByVoiceOver = false

  /// VoiceOver's elements: the layout's visible fields, labels and buttons.
  func present(_ elements: [Element]) {
    defer {
      if scrolledByVoiceOver {
        scrolledByVoiceOver = false
        UIAccessibility.post(notification: .pageScrolled, argument: nil)
      }
    }
    guard elements != presented else { return }
    presented = elements
    var next = [UInt64: CanvasElement]()
    var ordered = [CanvasElement]()
    for item in elements {
      let element = controls[item.id] ?? CanvasElement(accessibilityContainer: self)
      element.accessibilityLabel = item.label.isEmpty ? item.value : item.label
      element.accessibilityValue = item.label.isEmpty || item.value.isEmpty ? nil : item.value
      element.accessibilityFrameInContainerSpace = CGRect(
        x: item.left, y: item.top, width: item.width, height: item.height)
      switch item.kind {
      case .button: element.accessibilityTraits = .button
      case .label: element.accessibilityTraits = .staticText
      case .field: element.accessibilityTraits = .none
      }
      if !item.enabled && item.kind != .label { element.accessibilityTraits.insert(.notEnabled) }
      let id = item.id
      // An element's action can move the focus too, as a tap does.
      element.activate =
        item.enabled ? { [weak self] in self?.proxy.settle(then: .activate(id: id)) } : nil
      next[item.id] = element
      ordered.append(element)
    }
    controls = next
    isAccessibilityElement = elements.isEmpty
    accessibilityElements = ordered
  }
}

private final class CanvasElement: UIAccessibilityElement {
  var activate: (() -> Void)?

  override func accessibilityActivate() -> Bool {
    guard let activate else { return false }
    activate()
    return true
  }
}

/// A text field the user never sees: the keyboard and its input methods type
/// into it, and what they type goes to the engine's field, composing (marked)
/// text included. It holds nothing once text is committed.
private final class TextProxy: UITextField, UITextFieldDelegate {
  var send: ((Input) -> Void)?
  /// Find mode: Return performs the find (FileMaker Go's Go key).
  var finding = false {
    didSet {
      guard finding != oldValue else { return }
      returnKeyType = finding ? .search : .default
      if isFirstResponder { reloadInputViews() }
    }
  }
  /// The field takes a password: secure entry, nothing suggested or kept.
  var password = false {
    didSet {
      guard password != oldValue else { return }
      isSecureTextEntry = password
      textContentType = password ? .password : nil
      if isFirstResponder { reloadInputViews() }
    }
  }
  private var composing = false
  /// The engine asked for the keyboard to go: no Done to send.
  private var leaving = false

  init() {
    super.init(frame: CGRect(x: 0, y: 0, width: 1, height: 1))
    delegate = self
    textColor = .clear
    tintColor = .clear
    backgroundColor = .clear
    autocorrectionType = .no
    spellCheckingType = .no
    smartQuotesType = .no
    smartDashesType = .no
    smartInsertDeleteType = .no
    accessibilityElementsHidden = true
    addTarget(self, action: #selector(changed), for: .editingChanged)
    inputAccessoryView = editingBar()
  }

  required init?(coder: NSCoder) { nil }

  /// FileMaker Go's editing toolbar: the previous and next field, Done.
  private func editingBar() -> UIToolbar {
    let bar = UIToolbar(frame: CGRect(x: 0, y: 0, width: 320, height: 44))
    let previous = UIBarButtonItem(
      image: UIImage(systemName: "chevron.up"), primaryAction: UIAction { [weak self] _ in
        self?.settle(then: .press(key: .tab, shift: true, command: false))
      })
    previous.accessibilityLabel = "Previous field"
    let next = UIBarButtonItem(
      image: UIImage(systemName: "chevron.down"), primaryAction: UIAction { [weak self] _ in
        self?.settle(then: .press(key: .tab, shift: false, command: false))
      })
    next.accessibilityLabel = "Next field"
    let done = UIBarButtonItem(
      systemItem: .done, primaryAction: UIAction { [weak self] _ in
        self?.settle(then: .run(command: .done))
      })
    bar.items = [previous, next, .flexibleSpace(), done]
    bar.sizeToFit()
    return bar
  }

  // The field's text is the engine's: the edit menu's (and the keyboard's)
  // Select All, Copy and Cut act there. Paste puts the text into this field,
  // which sends it on as typing.
  override func selectAll(_ sender: Any?) { send?(.press(key: .a, shift: false, command: true)) }
  // A paste is the field's paste, not typing (a keystroke trigger does not
  // see it), of the document's last copy until that is on the pasteboard.
  override func paste(_ sender: Any?) {
    settle(then: .pasteCopied(clipboard: UIPasteboard.general.string ?? ""))
  }
  override func copy(_ sender: Any?) { send?(.press(key: .c, shift: false, command: true)) }
  override func cut(_ sender: Any?) { send?(.press(key: .x, shift: false, command: true)) }

  override func canPerformAction(_ action: Selector, withSender sender: Any?) -> Bool {
    switch action {
    case #selector(selectAll(_:)), #selector(copy(_:)), #selector(cut(_:)), #selector(paste(_:)):
      return true
    default: return super.canPerformAction(action, withSender: sender)
    }
  }

  /// Commit the input method's marked text as it shows, then send `input`:
  /// a save or a move to another field never takes a provisional candidate.
  func settle(then input: Input) {
    if markedTextRange != nil {
      unmarkText()
      changed()
    }
    send?(input)
  }

  @objc private func changed() {
    if let marked = markedTextRange {
      composing = true
      send?(.compose(text: text(in: marked) ?? ""))
      return
    }
    let committed = text ?? ""
    if !committed.isEmpty {
      send?(.text(text: committed))
    } else if composing {
      // The composition was cancelled.
      send?(.compose(text: ""))
    }
    composing = false
    text = ""
  }

  override func deleteBackward() {
    if (text ?? "").isEmpty {
      send?(.press(key: .backspace, shift: false, command: false))
    } else {
      super.deleteBackward()
    }
  }

  func textFieldShouldReturn(_ textField: UITextField) -> Bool {
    send?(finding ? .run(command: .performFind) : .press(key: .enter, shift: false, command: false))
    return false
  }

  func leave() {
    leaving = true
    resignFirstResponder()
    leaving = false
  }

  func textFieldDidEndEditing(_ textField: UITextField) {
    // The user put the keyboard away (iPad's keyboard key): the field is
    // left as with Done.
    if !leaving { settle(then: .run(command: .done)) }
  }

  override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
    var unhandled = Set<UIPress>()
    for press in presses {
      // Keys that edit or move go to the engine's field, unless an input
      // method is composing.
      // Plain Return is the text field's (textFieldShouldReturn, which also
      // performs a find in Find mode); Command-Return is Commit's shortcut.
      if markedTextRange == nil, let input = press.key.flatMap(Self.input(for:)),
        press.key?.keyCode != .keyboardReturnOrEnter
          || press.key?.modifierFlags.contains(.command) == true
      {
        send?(input)
      } else {
        unhandled.insert(press)
      }
    }
    if !unhandled.isEmpty { super.pressesBegan(unhandled, with: event) }
  }

  /// The engine's key for a hardware keyboard's key, if it is one.
  static func input(for key: UIKey) -> Input? {
    let engine: Key
    switch key.keyCode {
    case .keyboardTab: engine = .tab
    // Space types a space and selects a checkbox or radio button item.
    case .keyboardSpacebar: engine = .space
    // Select All, Copy, Cut and Undo (Shift: Redo) act on the field's text,
    // which is the engine's.
    case .keyboardA where key.modifierFlags.contains(.command): engine = .a
    case .keyboardC where key.modifierFlags.contains(.command): engine = .c
    case .keyboardX where key.modifierFlags.contains(.command): engine = .x
    case .keyboardZ where key.modifierFlags.contains(.command): engine = .z
    case .keyboardEscape: engine = .escape
    case .keyboardUpArrow: engine = .arrowUp
    case .keyboardDownArrow: engine = .arrowDown
    case .keyboardLeftArrow: engine = .arrowLeft
    case .keyboardRightArrow: engine = .arrowRight
    case .keyboardHome: engine = .home
    case .keyboardEnd: engine = .end
    case .keyboardPageUp: engine = .pageUp
    case .keyboardPageDown: engine = .pageDown
    case .keyboardDeleteForward: engine = .delete
    case .keyboardReturnOrEnter: engine = .enter
    // The keypad's Enter is FileMaker's Enter, not Return.
    case .keypadEnter: engine = .keypadEnter
    default: return nil
    }
    return .press(
      key: engine, shift: key.modifierFlags.contains(.shift),
      command: key.modifierFlags.contains(.command))
  }
}
