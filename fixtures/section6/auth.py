import requests as r


def verify_token(token):
    # Conceptually "the auth call" even though it never says "auth".
    return token and token.count(".") == 2


def login(username, password):
    return verify_token(issue_token(username, password))


def issue_token(username, password):
    return f"{username}.{hash(password)}.signature"


def compute_invoice_total(line_items):
    return sum(item["amount"] for item in line_items)


def render_banner(text):
    return f"== {text} =="
