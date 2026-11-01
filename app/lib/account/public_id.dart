/// Matches core public identifier Display: lowercase base32, without padding.
String publicId(List<int> bytes) {
  const alphabet = 'abcdefghijklmnopqrstuvwxyz234567';
  final output = StringBuffer();
  var buffer = 0, bits = 0;
  for (final byte in bytes) {
    buffer = (buffer << 8) | byte;
    bits += 8;
    while (bits >= 5) {
      bits -= 5;
      output.write(alphabet[(buffer >> bits) & 31]);
    }
    buffer &= (1 << bits) - 1;
  }
  if (bits > 0) output.write(alphabet[(buffer << (5 - bits)) & 31]);
  return output.toString();
}
