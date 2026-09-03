# Domain-specific fixtures: Sinatra-style Route (an HTTP-verb call with a
# string path and a block) and Response (`json`/`erb`/`redirect` — Sinatra's
# response-producing helpers).

get "/users" do
  json([{ id: 1 }])
end

post "/users" do
  status 201
  json({ created: true })
end

get "/health" do
  erb :health
end

delete "/users/:id" do
  redirect "/users"
end
