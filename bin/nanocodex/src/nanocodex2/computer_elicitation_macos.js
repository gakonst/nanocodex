// Local consent UI only. Provider strings are data, never executable source.
ObjC.import('AppKit');
function run(argv) {
  const request = JSON.parse(argv[0]);
  const app = $.NSApplication.sharedApplication;
  app.setActivationPolicy($.NSApplicationActivationPolicyAccessory);
  const alert = $.NSAlert.alloc.init;
  alert.messageText = 'Computer Use permission';
  alert.informativeText = 'Review the provider request below. Only you may approve it. Enter JSON form values if requested. Session permission covers this exact request in this live session.';
  alert.addButtonWithTitle('Cancel');
  alert.addButtonWithTitle('Decline');
  alert.addButtonWithTitle('Allow Once');
  if (request.sessionAllowed) alert.addButtonWithTitle('Allow This Session');
  const container = $.NSView.alloc.initWithFrame($.NSMakeRect(0,0,650,430));
  const scroll = $.NSScrollView.alloc.initWithFrame($.NSMakeRect(0,100,650,330));
  scroll.hasVerticalScroller = true;
  const details = $.NSTextView.alloc.initWithFrame($.NSMakeRect(0,0,630,330));
  details.editable = false;
  details.selectable = true;
  details.string = 'Request context:\n' + JSON.stringify(request.context, null, 2) + '\n\nProvider request (untrusted text):\n' + request.form;
  scroll.documentView = details;
  container.addSubview(scroll);
  const label = $.NSTextField.labelWithString('Form response (JSON; empty object for a permission-only request):');
  label.frame = $.NSMakeRect(0,72,650,24);
  container.addSubview(label);
  const content = $.NSTextField.alloc.initWithFrame($.NSMakeRect(0,8,650,60));
  content.stringValue = '{}';
  container.addSubview(content);
  alert.accessoryView = container;
  app.activateIgnoringOtherApps(true);
  const button = Number(alert.runModal);
  const actions = ['cancel', 'decline', 'accept', 'accept-session'];
  const action = actions[button - 1000] || 'cancel';
  return JSON.stringify({action, content: ObjC.unwrap(content.stringValue)});
}
