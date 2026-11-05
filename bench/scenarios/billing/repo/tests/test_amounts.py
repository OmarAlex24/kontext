import unittest

from billing.amounts import format_amount, to_cents


class Amounts(unittest.TestCase):
    def test_to_cents(self):
        self.assertEqual(to_cents("1,250.50"), 125050)
        self.assertEqual(to_cents("$3"), 300)
        self.assertEqual(to_cents("0.5"), 50)

    def test_format_amount(self):
        self.assertEqual(format_amount(125050), "$1,250.50")
        self.assertEqual(format_amount(-5), "-$0.05")


if __name__ == "__main__":
    unittest.main()
