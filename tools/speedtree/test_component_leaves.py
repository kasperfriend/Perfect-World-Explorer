import struct
import unittest
from compare_component_leaves import leaf_hashes


def fixture():
    u = lambda v: struct.pack('<I', v)
    f = lambda v: struct.pack('<f', v)
    return (b'PWCOMP02' + u(0) * 5 + u(0) + u(0) + u(1) + u(1)
            + b''.join(f(float(i)) for i in range(6)) + u(4)
            + b''.join(f(float(i)) for i in range(24)))


class ComponentLeafHashTests(unittest.TestCase):
    def test_hash_and_truncation(self):
        data = fixture()
        self.assertEqual(leaf_hashes(data), ['bd59677d3a21e136'])
        for i in range(len(data)):
            with self.subTest(i=i), self.assertRaises(ValueError):
                leaf_hashes(data[:i])
        with self.assertRaises(ValueError):
            leaf_hashes(data + b'X')

    def test_cluster_range(self):
        data = bytearray(fixture())
        data[68:72] = struct.pack('<I', 256)
        with self.assertRaises(ValueError):
            leaf_hashes(data)


if __name__ == '__main__':
    unittest.main()
