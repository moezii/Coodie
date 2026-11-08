; Sinatra-style bare routes: get "/users" do ... end
(call
  method: (identifier) @reg.method
  arguments: (argument_list) @reg.args) @reg.site

; Receiver routes: app.get "/users", Rails router draws
(call
  receiver: (_) @reg.receiver
  method: (identifier) @reg.method
  arguments: (argument_list) @reg.args) @reg.site
