function publishEvent(payload) {
  return fetch("https://audit.example/events", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

function loadUser(id) {
  return fetch(`/users/${id}`, { method: "GET" });
}

function formatDate(value) {
  return new Date(value).toISOString();
}
