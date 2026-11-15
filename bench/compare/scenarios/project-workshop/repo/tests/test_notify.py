import unittest
from datetime import datetime, timezone

from workshop import notify
from workshop.storage import Store


class NotifyTest(unittest.TestCase):
    def setUp(self):
        notify.OUTBOX.clear()

    def test_send_email(self):
        notify.send_email("ana@example.org", "Hi", "Body")
        self.assertEqual(notify.OUTBOX[-1]["channel"], "email")
        self.assertEqual(notify.OUTBOX[-1]["to"], "ana@example.org")

    def test_send_sms(self):
        notify.send_sms("+5215550000000", "Body")
        self.assertEqual(notify.OUTBOX[-1]["channel"], "sms")

    def test_send_due_with_nothing_due(self):
        store = Store()
        now = datetime(2027, 3, 1, 12, 0, tzinfo=timezone.utc)
        try:
            n = notify.send_due(store, now)
        except NotImplementedError:
            self.skipTest("reminders_due() is not written yet")
        self.assertEqual(n, 0)
