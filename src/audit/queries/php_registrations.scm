; Router calls: $app->get('/users', $handler); Route::get('/users', ...)
(member_call_expression
  object: (_) @reg.receiver
  name: (name) @reg.method
  arguments: (arguments) @reg.args) @reg.site

(scoped_call_expression
  scope: (_) @reg.receiver
  name: (name) @reg.method
  arguments: (arguments) @reg.args) @reg.site
