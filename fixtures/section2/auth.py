import requests as r


def verify_token(token):
    return token and token.count(".") == 2


def publish_login_event(payload):
    ignored = "r.post('/not-real')"
    # r.post('/also-not-real')
    return r.post("https://audit.example/events", json=payload)
