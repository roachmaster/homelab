import unittest

from command_policy import CommandError, parse_command


class CommandPolicyTest(unittest.TestCase):
    def test_pipeline_uses_fixed_executable_paths(self):
        self.assertEqual(
            parse_command("sort names.txt | uniq", {"names.txt"}),
            [["/usr/bin/sort", "--", "names.txt"], ["/usr/bin/uniq"]],
        )

    def test_shell_syntax_and_unapproved_arguments_are_rejected(self):
        commands = [
            "sort /etc/passwd",
            "sort ../names.txt",
            "sort names.txt; id",
            "sort names.txt > output.txt",
            "sort $(id) names.txt",
            "sort $HOME names.txt",
            "sort names.txt | cat",
            "sort --output=names.txt names.txt",
            "grep -f names.txt names.txt",
            "/usr/bin/sort names.txt",
            "sort names.txt\nid",
        ]
        for command in commands:
            with self.subTest(command=command), self.assertRaises(CommandError):
                parse_command(command, {"names.txt"})


if __name__ == "__main__":
    unittest.main()
