const { formatDate: fd } = require('./util/format');
const util = require('./util/format');

function start() {
  fd();
  util.slug();
  return <div className="x"><Button label="x" /></div>;
}

export default function () {}

module.exports = { start };
