"""Sending emails. Every email goes out from here."""


class Postmark:
    """Minimal Postmark client. In tests it's used with `sent` in memory."""

    def __init__(self, token="", sender="billing@example.com"):
        self.token = token
        self.sender = sender
        self.sent = []

    def send(self, to, subject, text):
        self.sent.append({"to": to, "subject": subject, "text": text})
        return True
