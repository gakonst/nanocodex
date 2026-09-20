// Local consent UI only. Provider strings are data, never executable source.
ObjC.import('AppKit');
function run(argv) {
  const request = JSON.parse(argv[0]);
  const form = JSON.parse(request.form);
  const fields = form.requestedSchema && form.requestedSchema.properties || {};
  const hasFields = Object.keys(fields).length > 0;
  const app = $.NSApplication.sharedApplication;
  app.setActivationPolicy($.NSApplicationActivationPolicyAccessory);
  const alert = $.NSAlert.alloc.init;
  alert.messageText = typeof form.message === 'string' ? form.message : 'Sky requests permission';
  alert.informativeText = typeof (form._meta && form._meta.subtitle) === 'string' ? form._meta.subtitle : '';
  alert.addButtonWithTitle('Cancel');
  alert.addButtonWithTitle('Decline');
  alert.addButtonWithTitle('Allow Once');
  if (request.sessionAllowed) alert.addButtonWithTitle('Allow This Session');
  let input = null;
  if (hasFields) {
    const container = $.NSView.alloc.initWithFrame($.NSMakeRect(0,0,650,430));
    const scroll = $.NSScrollView.alloc.initWithFrame($.NSMakeRect(0,100,650,330));
    scroll.hasVerticalScroller = true;
    const details = $.NSTextView.alloc.initWithFrame($.NSMakeRect(0,0,630,330));
    details.editable = false;
    details.selectable = true;
    details.string = JSON.stringify(form.requestedSchema, null, 2);
    scroll.documentView = details;
    container.addSubview(scroll);
    const label = $.NSTextField.labelWithString('Requested form values (JSON):');
    label.frame = $.NSMakeRect(0,72,650,24);
    container.addSubview(label);
    input = $.NSTextField.alloc.initWithFrame($.NSMakeRect(0,8,650,60));
    input.stringValue = '{}';
    container.addSubview(input);
    alert.accessoryView = container;
  }
  app.activateIgnoringOtherApps(true);
  const button = Number(alert.runModal);
  const actions = ['cancel', 'decline', 'accept', 'accept-session'];
  const action = actions[button - 1000] || 'cancel';
  return JSON.stringify({action, content: input ? ObjC.unwrap(input.stringValue) : '{}'});
}
