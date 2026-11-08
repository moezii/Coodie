function createUser(payload) {
  return fetch("/users", { method: "POST", body: JSON.stringify(payload) });
}

function loadUser() {
  return fetch("/users", { method: "GET" });
}
