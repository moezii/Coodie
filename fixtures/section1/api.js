function fetchUser() {
  return true;
}

class ApiClient {
  postData() {
    return fetch("/users");
  }
}

const authenticate = () => true;
